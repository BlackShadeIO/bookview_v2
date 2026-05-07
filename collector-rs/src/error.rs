#[derive(Debug, thiserror::Error)]
pub enum CollectorError {
    #[error("WebSocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Writer channel closed")]
    WriterClosed,
    #[allow(dead_code)]
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("Polymarket collector failed: {0}")]
    Polymarket(anyhow::Error),
    #[error("Binance collector failed: {0}")]
    Binance(anyhow::Error),
    #[error("Both collectors failed: poly={0}, binance={1}")]
    Both(anyhow::Error, anyhow::Error),
    #[error("Worker setup failed: {0}")]
    Setup(anyhow::Error),
}
