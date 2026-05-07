use alloy::primitives::{Address, B256, U256, address, keccak256};
use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolCall;
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE as BASE64_URL_SAFE, URL_SAFE_NO_PAD as BASE64_URL_SAFE_NOPAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::config::AppConfig;

const PROXY_FACTORY: Address = address!("0xaB45c5A4B0c941a2F231C04C3f49182e1A254052");
const RELAY_HUB: Address = address!("0xD216153c06E857cD7f72665E0aF1d7D82172F494");
const DEFAULT_GAS_LIMIT: u64 = 500_000;

alloy::sol! {
    struct ProxyCall {
        uint8 typeCode;
        address to;
        uint256 value;
        bytes data;
    }

    interface IProxyWallet {
        function proxy(ProxyCall[] memory calls) external payable returns (bytes[] memory returnValues);
    }
}

#[derive(Debug, Deserialize)]
pub struct RelayPayload {
    pub address: String,
    pub nonce: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerResponse {
    pub transaction_id: Option<String>,
    #[serde(rename = "transactionID")]
    pub transaction_id_alt: Option<String>,
    pub state: Option<String>,
    pub hash: Option<String>,
    pub transaction_hash: Option<String>,
}

impl RelayerResponse {
    pub fn tx_id(&self) -> Option<&str> {
        self.transaction_id.as_deref()
            .or(self.transaction_id_alt.as_deref())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignatureParams {
    gas_price: String,
    gas_limit: String,
    relayer_fee: String,
    relay_hub: String,
    relay: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitRequest {
    #[serde(rename = "type")]
    type_: String,
    from: String,
    to: String,
    proxy_wallet: String,
    data: String,
    nonce: String,
    signature: String,
    signature_params: SignatureParams,
    metadata: String,
}

pub struct RelayerClient {
    host: String,
    signer: PrivateKeySigner,
    proxy_wallet: Address,
    builder_key: String,
    builder_secret: String,
    builder_passphrase: String,
    http: reqwest::Client,
}

impl RelayerClient {
    pub fn new(config: &AppConfig, signer: PrivateKeySigner, http: reqwest::Client) -> Result<Self> {
        let key = config.relayer_api_key.as_ref()
            .ok_or_else(|| anyhow!("RELAYER_API_KEY required for proxy wallet CTF operations"))?;
        let secret = config.relayer_api_secret.as_ref()
            .ok_or_else(|| anyhow!("RELAYER_API_SECRET required for proxy wallet CTF operations"))?;
        let passphrase = config.relayer_passphrase.as_ref()
            .ok_or_else(|| anyhow!("RELAYER_PASSPHRASE required for proxy wallet CTF operations"))?;

        Ok(Self {
            host: config.relayer_host.clone(),
            signer,
            proxy_wallet: config.wallet_address,
            builder_key: key.expose().to_string(),
            builder_secret: secret.expose().to_string(),
            builder_passphrase: passphrase.expose().to_string(),
            http,
        })
    }

    fn hmac_signature(&self, timestamp: u64, method: &str, path: &str, body: Option<&str>) -> String {
        let mut message = format!("{timestamp}{method}{path}");
        if let Some(b) = body {
            message.push_str(b);
        }
        let secret_bytes = BASE64_URL_SAFE.decode(&self.builder_secret)
            .or_else(|_| BASE64_URL_SAFE_NOPAD.decode(&self.builder_secret))
            .or_else(|_| BASE64.decode(&self.builder_secret))
            .expect("RELAYER_API_SECRET must be valid base64");
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret_bytes)
            .expect("HMAC key length is valid");
        mac.update(message.as_bytes());
        let result = mac.finalize().into_bytes();
        BASE64_URL_SAFE.encode(result)
    }

    async fn get_relay_payload(&self) -> Result<RelayPayload> {
        let url = format!(
            "{}/relay-payload?address={}&type=PROXY",
            self.host,
            self.signer.address()
        );
        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("relay-payload failed ({status}): {body}");
        }
        resp.json().await.context("parsing relay-payload response")
    }

    fn build_struct_hash(
        &self,
        encoded_data: &[u8],
        gas_limit: u64,
        nonce: &str,
        relay_address: Address,
    ) -> B256 {
        let nonce_val: u64 = match nonce.parse() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(nonce = %nonce, error = %e, "Failed to parse relay nonce, falling back to 0");
                0
            }
        };
        let mut packed = Vec::with_capacity(256);
        packed.extend_from_slice(b"rlx:");
        packed.extend_from_slice(self.signer.address().as_slice());
        packed.extend_from_slice(PROXY_FACTORY.as_slice());
        packed.extend_from_slice(encoded_data);
        packed.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        packed.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        packed.extend_from_slice(&U256::from(gas_limit).to_be_bytes::<32>());
        packed.extend_from_slice(&U256::from(nonce_val).to_be_bytes::<32>());
        packed.extend_from_slice(RELAY_HUB.as_slice());
        packed.extend_from_slice(relay_address.as_slice());
        keccak256(&packed)
    }

    pub async fn execute(
        &self,
        calls: Vec<ProxyCall>,
        metadata: &str,
    ) -> Result<RelayerResponse> {
        let payload = self.get_relay_payload().await?;
        let relay_address: Address = payload.address.parse()
            .context("invalid relay address")?;

        tracing::info!(
            relay = %payload.address,
            nonce = %payload.nonce,
            "Got relay payload"
        );

        let encoded_data = IProxyWallet::proxyCall { calls }.abi_encode();

        let struct_hash = self.build_struct_hash(
            &encoded_data,
            DEFAULT_GAS_LIMIT,
            &payload.nonce,
            relay_address,
        );

        let sig = self.signer.sign_message(struct_hash.as_slice()).await
            .map_err(|e| anyhow!("signing failed: {e}"))?;
        let sig_hex = format!("0x{}", alloy::hex::encode(sig.as_bytes()));

        let request = SubmitRequest {
            type_: "PROXY".into(),
            from: format!("{}", self.signer.address()),
            to: format!("{PROXY_FACTORY}"),
            proxy_wallet: format!("{}", self.proxy_wallet),
            data: format!("0x{}", alloy::hex::encode(&encoded_data)),
            nonce: payload.nonce,
            signature: sig_hex,
            signature_params: SignatureParams {
                gas_price: "0".into(),
                gas_limit: DEFAULT_GAS_LIMIT.to_string(),
                relayer_fee: "0".into(),
                relay_hub: format!("{RELAY_HUB}"),
                relay: payload.address,
            },
            metadata: metadata.into(),
        };

        let body = serde_json::to_string(&request)?;

        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let hmac_sig = self.hmac_signature(timestamp, "POST", "/submit", Some(&body));

        let resp = self.http
            .post(format!("{}/submit", self.host))
            .header("POLY_BUILDER_API_KEY", &self.builder_key)
            .header("POLY_BUILDER_TIMESTAMP", timestamp.to_string())
            .header("POLY_BUILDER_PASSPHRASE", &self.builder_passphrase)
            .header("POLY_BUILDER_SIGNATURE", &hmac_sig)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;

        let status = resp.status();
        let resp_text = resp.text().await.unwrap_or_default();

        tracing::info!("submit response ({status}): {resp_text}");

        if !status.is_success() {
            anyhow::bail!("relayer submit failed ({status}): {resp_text}");
        }

        serde_json::from_str(&resp_text)
            .context(format!("parsing submit response: {resp_text}"))
    }

    pub async fn poll_until_complete(&self, tx_id: &str) -> Result<RelayerResponse> {
        for i in 0..60 {
            let url = format!("{}/transaction?id={}", self.host, tx_id);
            let resp = self.http.get(&url).send().await?;
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();

            if !status.is_success() {
                tracing::debug!(status = %status, "poll attempt {i} non-success, retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            let json: serde_json::Value = serde_json::from_str(&body)
                .context(format!("parsing poll response: {body}"))?;

            let entry = if json.is_array() {
                json.get(0).cloned().unwrap_or(serde_json::Value::Null)
            } else {
                json
            };

            let state = entry.get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let tx_hash = entry.get("transactionHash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if i == 0 || (i > 0 && i % 5 == 0) {
                tracing::info!(state = %state, tx_hash = %tx_hash, "poll attempt {i}");
            }

            match state {
                "STATE_CONFIRMED" | "STATE_MINED" => {
                    return Ok(RelayerResponse {
                        transaction_id: Some(tx_id.to_string()),
                        transaction_id_alt: None,
                        state: Some(state.to_string()),
                        hash: Some(tx_hash.clone()),
                        transaction_hash: Some(tx_hash),
                    });
                }
                "STATE_FAILED" => anyhow::bail!("Transaction failed: {body}"),
                "STATE_INVALID" => anyhow::bail!("Transaction invalid: {body}"),
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        anyhow::bail!("Transaction did not confirm within timeout")
    }

    pub async fn execute_and_wait(
        &self,
        calls: Vec<ProxyCall>,
        metadata: &str,
    ) -> Result<RelayerResponse> {
        let resp = self.execute(calls, metadata).await?;
        let tx_id = resp.tx_id()
            .ok_or_else(|| anyhow!("no transaction ID in response"))?
            .to_string();

        tracing::info!(tx_id = %tx_id, "Transaction submitted, waiting for confirmation...");
        self.poll_until_complete(&tx_id).await
    }
}
