use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::auth::Normal;
use thiserror::Error;

use super::config::{AppConfig, SignatureType};

pub type AuthenticatedClobClient = polymarket_client_sdk_v2::clob::Client<Authenticated<Normal>>;

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("invalid private key")]
    InvalidPrivateKey,
    #[error("authentication failed: {0}")]
    AuthFailed(String),
}

pub fn create_signer(config: &AppConfig) -> Result<PrivateKeySigner, AuthError> {
    let key_hex = config.private_key.expose();
    let key_hex = key_hex.strip_prefix("0x").unwrap_or(key_hex);

    let mut signer: PrivateKeySigner = key_hex
        .parse()
        .map_err(|_| AuthError::InvalidPrivateKey)?;

    signer.set_chain_id(Some(config.chain_id));

    tracing::info!(address = %signer.address(), chain_id = config.chain_id, "Executor signer created");
    Ok(signer)
}

pub async fn create_authenticated_client(
    config: &AppConfig,
    signer: &PrivateKeySigner,
) -> Result<AuthenticatedClobClient, AuthError> {
    let client = polymarket_client_sdk_v2::clob::Client::new(
        &config.clob_host,
        polymarket_client_sdk_v2::clob::Config::default(),
    )
    .map_err(|e| AuthError::AuthFailed(e.to_string()))?;

    let sig_type = match config.signature_type {
        SignatureType::Eoa => polymarket_client_sdk_v2::clob::types::SignatureType::Eoa,
        SignatureType::Proxy => polymarket_client_sdk_v2::clob::types::SignatureType::Proxy,
        SignatureType::GnosisSafe => polymarket_client_sdk_v2::clob::types::SignatureType::GnosisSafe,
        SignatureType::Poly1271 => polymarket_client_sdk_v2::clob::types::SignatureType::Poly1271,
    };

    let mut builder = client
        .authentication_builder(signer)
        .signature_type(sig_type);

    if let Some(funder) = config.funder_address {
        builder = builder.funder(funder);
    }

    let authenticated = builder
        .authenticate()
        .await
        .map_err(|e| AuthError::AuthFailed(e.to_string()))?;

    tracing::info!(address = %authenticated.address(), "Executor CLOB client authenticated");

    Ok(authenticated)
}
