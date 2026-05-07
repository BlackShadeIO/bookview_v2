use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use crate::error::CollectorError;
use crate::types::Line;

pub struct SeqCounter(AtomicU64);

impl SeqCounter {
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

#[derive(Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<Line>,
}

impl WriterHandle {
    pub fn send(&self, line: Line) -> Result<(), CollectorError> {
        self.tx
            .try_send(line)
            .map_err(|_| CollectorError::WriterClosed)
    }
}

pub fn spawn_writer(path: PathBuf) -> (WriterHandle, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<Line>(65_536);
    let handle = tokio::task::spawn_blocking(move || {
        writer_loop(rx, &path);
    });
    (WriterHandle { tx }, handle)
}

fn writer_loop(mut rx: mpsc::Receiver<Line>, path: &PathBuf) {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("Failed to open {}: {e}", path.display()));

    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    let mut buf = Vec::with_capacity(4096);

    while let Some(line) = rx.blocking_recv() {
        serialize_and_write(&mut buf, &mut writer, &line);

        while let Ok(line) = rx.try_recv() {
            serialize_and_write(&mut buf, &mut writer, &line);
        }

        writer.flush().unwrap();
    }

    writer.flush().unwrap();
}

fn serialize_and_write(buf: &mut Vec<u8>, writer: &mut BufWriter<std::fs::File>, line: &Line) {
    buf.clear();
    serde_json::to_writer(&mut *buf, line).unwrap();
    buf.push(b'\n');
    writer.write_all(buf).unwrap();
}
