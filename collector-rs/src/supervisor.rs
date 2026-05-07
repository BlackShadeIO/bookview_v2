use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use bookview_collector::config::*;
use crate::worker::run_worker;

pub fn next_market_start(now_secs: f64) -> i64 {
    let adjusted = now_secs + PRE_START_SECONDS as f64;
    let boundary = (adjusted / MARKET_INTERVAL as f64).ceil() as i64;
    boundary * MARKET_INTERVAL as i64
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

pub async fn run(shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut launched: HashSet<i64> = HashSet::new();
    let mut workers: JoinSet<(i64, Result<(), bookview_collector::error::WorkerError>)> = JoinSet::new();
    let mut restart_counts: HashMap<i64, u32> = HashMap::new();

    loop {
        let now = now_secs();
        let target = next_market_start(now);
        let launch_at = target as f64 - PRE_START_SECONDS as f64;
        let sleep_secs = launch_at - now_secs();

        if sleep_secs > 0.0 {
            tracing::info!(
                market = target,
                sleep_secs = format!("{:.1}", sleep_secs),
                "Next market, sleeping"
            );
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(sleep_secs)) => {}
                _ = shutdown.cancelled() => break,
            }
        }

        if shutdown.is_cancelled() {
            break;
        }

        if !launched.contains(&target) {
            tracing::info!(market = target, "Spawning worker");
            let cancel = shutdown.child_token();
            let ms = target;
            workers.spawn(async move {
                let result = run_worker(ms, cancel).await;
                (ms, result)
            });
            launched.insert(target);
        }

        // Reap finished workers (non-blocking)
        while let Some(Ok((ms, result))) = workers.try_join_next() {
            match &result {
                Ok(()) => {
                    tracing::info!(market = ms, "Worker finished normally");
                }
                Err(e) => {
                    let now = now_secs();
                    let expected_stop = ms as f64 + (MARKET_DURATION + TAIL_SECONDS) as f64;
                    let count = restart_counts.entry(ms).or_insert(0);

                    if now < expected_stop && *count < MAX_RESTARTS {
                        *count += 1;
                        tracing::warn!(
                            market = ms,
                            error = %e,
                            attempt = *count,
                            "Worker died early, restarting"
                        );
                        let cancel = shutdown.child_token();
                        workers.spawn(async move {
                            let result = run_worker(ms, cancel).await;
                            (ms, result)
                        });
                    } else {
                        tracing::error!(
                            market = ms,
                            error = %e,
                            "Worker failed, max restarts reached"
                        );
                    }
                }
            }
        }

        // Prune launched set (keep last hour)
        let cutoff = (now_secs() - 3600.0) as i64;
        launched.retain(|&ms| ms > cutoff);
    }

    // Shutdown: cancel propagates to all child tokens automatically.
    // Wait for workers to finish.
    let active = workers.len();
    if active > 0 {
        tracing::info!(count = active, "Waiting for active workers to finish");
    }
    while let Some(result) = workers.join_next().await {
        match result {
            Ok((ms, Ok(()))) => {
                tracing::info!(market = ms, "Worker exited cleanly during shutdown");
            }
            Ok((ms, Err(e))) => {
                tracing::warn!(market = ms, error = %e, "Worker errored during shutdown");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Worker task panicked during shutdown");
            }
        }
    }

    Ok(())
}
