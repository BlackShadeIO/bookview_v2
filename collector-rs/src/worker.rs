use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::binance::run_binance;
use crate::polymarket::run_polymarket;
use bookview_collector::config::*;
use bookview_collector::error::WorkerError;
use bookview_collector::fair_value::{self, FairValueFeeds, FairValueParams};
use bookview_collector::types::*;
use bookview_collector::writer::{SeqCounter, spawn_writer};

pub async fn run_worker(market_start: i64, cancel: CancellationToken) -> Result<(), WorkerError> {
    let folder = DATA_DIR.join(market_start.to_string());
    tokio::fs::create_dir_all(&folder)
        .await
        .map_err(|e| WorkerError::Setup(e.into()))?;

    let clob_path = folder.join(CLOB_FILENAME);
    let depth_path = folder.join(DEPTH_FILENAME);

    let (clob_writer, clob_handle) = spawn_writer(clob_path);
    let (depth_writer, depth_handle) = spawn_writer(depth_path);

    let fv_path = folder.join(FV_FILENAME);
    let (fv_writer, fv_handle) = spawn_writer(fv_path);
    let (fv_feeds, fv_receivers) = FairValueFeeds::create();

    let clob_seq = Arc::new(SeqCounter::new());
    let depth_seq = Arc::new(SeqCounter::new());

    let stop_at = market_start + MARKET_DURATION as i64 + TAIL_SECONDS as i64;
    let pid = std::process::id();

    // Startup markers
    let _ = clob_writer.send(make_system_line(
        market_start,
        "polymarket",
        clob_seq.next(),
        json!({"market_start": market_start, "pid": pid, "stop_at": stop_at}),
        "worker_started",
    ));
    let _ = depth_writer.send(make_system_line(
        market_start,
        "binance",
        depth_seq.next(),
        json!({"market_start": market_start, "pid": pid, "stop_at": stop_at}),
        "worker_started",
    ));

    // Timer: cancel after market window expires
    let timer_cancel = cancel.clone();
    let timer = tokio::spawn(async move {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let remaining = (stop_at - now).max(0) as u64;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(remaining)) => {
                timer_cancel.cancel();
            }
            _ = timer_cancel.cancelled() => {}
        }
    });

    // Shared HTTP client
    let client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .build()
        .expect("failed to build HTTP client");

    // Executor setup (conditional on feature + config)
    #[cfg(feature = "executor")]
    let executor_config = bookview_collector::executor::config::AppConfig::try_from_env();

    #[cfg(feature = "executor")]
    let (market_info_tx, poly_bba_tx, fv_snapshot_tx, executor_setup) = if let Some(exec_config) = executor_config {
        let (mi_tx, mi_rx) = watch::channel::<Option<MarketInfo>>(None);
        let (pb_tx, pb_rx) = watch::channel::<Option<PolyBbaSnapshot>>(None);
        let (fv_tx, fv_rx) = watch::channel::<Option<FairValueSnapshot>>(None);

        (
            Some(mi_tx),
            Some(Arc::new(pb_tx)),
            Some(fv_tx),
            Some((mi_rx, pb_rx, fv_rx, exec_config)),
        )
    } else {
        (None, None, None, None)
    };

    #[cfg(not(feature = "executor"))]
    let (market_info_tx, poly_bba_tx, fv_snapshot_tx): (
        Option<watch::Sender<Option<MarketInfo>>>,
        Option<Arc<watch::Sender<Option<PolyBbaSnapshot>>>>,
        Option<watch::Sender<Option<FairValueSnapshot>>>,
    ) = (None, None, None);

    // Fair value engine task
    let fv_cancel = cancel.clone();
    let fv_writer_clone = fv_writer.clone();
    let fv_task = tokio::spawn(async move {
        fair_value::run_fair_value(
            market_start,
            fv_receivers.bba,
            fv_receivers.depth,
            fv_receivers.trade,
            fv_receivers.strike,
            fv_writer_clone,
            fv_cancel,
            FairValueParams::default(),
            Some(MODEL_DIR.clone()),
            fv_snapshot_tx,
        )
        .await
    });

    // Run collectors concurrently
    let poly_cancel = cancel.clone();
    let poly_writer = clob_writer.clone();
    let poly_client = client.clone();
    let poly = tokio::spawn(async move {
        run_polymarket(market_start, poly_writer, poly_cancel, poly_client, market_info_tx, poly_bba_tx).await
    });

    let bin_cancel = cancel.clone();
    let bin_writer = depth_writer.clone();
    let bin = tokio::spawn(async move {
        run_binance(market_start, bin_writer, bin_cancel, client, Some(fv_feeds)).await
    });

    // Spawn executor task if configured
    #[cfg(feature = "executor")]
    let executor_task = if let Some((mi_rx, pb_rx, fv_rx, exec_config)) = executor_setup {
        let (exec_writer, exec_handle) = spawn_writer(folder.join("executor.jsonl"));
        let exec_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            bookview_collector::executor::runner::run_executor(
                market_start,
                mi_rx,
                pb_rx,
                fv_rx,
                exec_writer,
                exec_cancel,
                exec_config,
            )
            .await
        });
        Some((task, exec_handle))
    } else {
        None
    };

    let (poly_res, bin_res) = tokio::join!(poly, bin);

    // Abort FV and executor tasks
    fv_task.abort();
    let _ = fv_task.await;

    #[cfg(feature = "executor")]
    if let Some((task, handle)) = executor_task {
        task.abort();
        let _ = task.await;
        drop(handle);
    }

    timer.abort();

    // Check for errors
    let poly_err = poly_res
        .map_err(|e| anyhow::anyhow!("polymarket task panicked: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!(e)));
    let bin_err = bin_res
        .map_err(|e| anyhow::anyhow!("binance task panicked: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!(e)));

    if let Err(ref e) = poly_err {
        tracing::error!(market_start, error = %e, "Polymarket collector failed");
        let _ = clob_writer.send(make_line(
            market_start,
            "polymarket",
            "error",
            clob_seq.next(),
            json!({"error": e.to_string(), "type": "CollectorError"}),
            json!({"reason": "worker_exception"}),
        ));
    }
    if let Err(ref e) = bin_err {
        tracing::error!(market_start, error = %e, "Binance collector failed");
        let _ = depth_writer.send(make_line(
            market_start,
            "binance",
            "error",
            depth_seq.next(),
            json!({"error": e.to_string(), "type": "CollectorError"}),
            json!({"reason": "worker_exception"}),
        ));
    }

    // Always: shutdown markers
    let _ = clob_writer.send(make_line(
        market_start,
        "polymarket",
        "collector_stopped",
        clob_seq.next(),
        serde_json::Value::Null,
        json!({"market_start": market_start, "pid": pid}),
    ));
    let _ = depth_writer.send(make_line(
        market_start,
        "binance",
        "collector_stopped",
        depth_seq.next(),
        serde_json::Value::Null,
        json!({"market_start": market_start, "pid": pid}),
    ));

    // Drop senders to close channels, then wait for writers to drain
    drop(clob_writer);
    drop(depth_writer);
    drop(fv_writer);
    let _ = clob_handle.await;
    let _ = depth_handle.await;
    let _ = fv_handle.await;

    match (poly_err, bin_err) {
        (Err(p), Err(b)) => Err(WorkerError::Both(p, b)),
        (Err(p), Ok(())) => Err(WorkerError::Polymarket(p)),
        (Ok(()), Err(b)) => Err(WorkerError::Binance(b)),
        (Ok(()), Ok(())) => Ok(()),
    }
}
