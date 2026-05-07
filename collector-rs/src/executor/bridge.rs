use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("bridge request failed: {0}")]
    RequestFailed(String),
    #[error("bridge response parse error: {0}")]
    ParseError(String),
}

#[derive(Debug, Clone, Serialize)]
struct DepositRequest {
    address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DepositAddresses {
    pub evm: Option<String>,
    pub svm: Option<String>,
    pub btc: Option<String>,
}

pub async fn fetch_deposit_addresses(
    bridge_host: &str,
    wallet_address: Address,
) -> Result<DepositAddresses, BridgeError> {
    let url = format!("{bridge_host}/deposit");
    let body = DepositRequest {
        address: format!("{wallet_address:#x}"),
    };

    tracing::info!(url = %url, wallet = %wallet_address, "Fetching deposit addresses");

    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BridgeError::RequestFailed(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(BridgeError::RequestFailed(format!("HTTP {status}: {text}")));
    }

    let addrs: DepositAddresses = resp
        .json()
        .await
        .map_err(|e| BridgeError::ParseError(e.to_string()))?;

    tracing::info!(?addrs, "Deposit addresses fetched");
    Ok(addrs)
}
