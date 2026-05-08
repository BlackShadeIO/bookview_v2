use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("missing required env var: {0}")]
    Missing(String),
    #[error("invalid address format for {name}: {value}")]
    InvalidAddress { name: String, value: String },
    #[error("invalid value for {name}: {msg}")]
    InvalidValue { name: String, msg: String },
}

pub struct SecretString(String);

impl SecretString {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureType {
    Eoa = 0,
    Proxy = 1,
    GnosisSafe = 2,
    Poly1271 = 3,
}

impl FromStr for SignatureType {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "0" | "eoa" | "EOA" => Ok(Self::Eoa),
            "1" | "proxy" | "PROXY" => Ok(Self::Proxy),
            "2" | "gnosis_safe" | "GNOSIS_SAFE" => Ok(Self::GnosisSafe),
            "3" | "poly1271" | "POLY1271" => Ok(Self::Poly1271),
            _ => Err(ConfigError::InvalidValue {
                name: "SIGNATURE_TYPE".into(),
                msg: format!("unknown signature type: {s}"),
            }),
        }
    }
}

pub struct AppConfig {
    pub private_key: SecretString,
    pub wallet_address: alloy::primitives::Address,
    pub funder_address: Option<alloy::primitives::Address>,
    pub signature_type: SignatureType,
    pub clob_host: String,
    pub gamma_host: String,
    pub bridge_host: String,
    pub rpc_url: String,
    pub chain_id: u64,
    pub dry_run: bool,
    pub enable_test_order: bool,
    pub max_order_size: Decimal,
    pub max_position_size: Decimal,
    pub order_price_min: Decimal,
    pub order_price_max: Decimal,
    pub relayer_host: String,
    pub relayer_api_key: Option<SecretString>,
    pub relayer_api_secret: Option<SecretString>,
    pub relayer_passphrase: Option<SecretString>,
    pub strategy_name: String,
    pub split_amount: u64,
    pub strategy_order_size: f64,
    pub max_markets: u64,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("private_key", &self.private_key)
            .field("wallet_address", &self.wallet_address)
            .field("funder_address", &self.funder_address)
            .field("signature_type", &self.signature_type)
            .field("clob_host", &self.clob_host)
            .field("gamma_host", &self.gamma_host)
            .field("bridge_host", &self.bridge_host)
            .field("rpc_url", &self.rpc_url)
            .field("chain_id", &self.chain_id)
            .field("dry_run", &self.dry_run)
            .field("enable_test_order", &self.enable_test_order)
            .field("max_order_size", &self.max_order_size)
            .field("max_position_size", &self.max_position_size)
            .field("order_price_min", &self.order_price_min)
            .field("order_price_max", &self.order_price_max)
            .field("relayer_host", &self.relayer_host)
            .field("relayer_api_key", &self.relayer_api_key.as_ref().map(|_| "[SET]"))
            .field("relayer_api_secret", &self.relayer_api_secret.as_ref().map(|_| "[SET]"))
            .field("relayer_passphrase", &self.relayer_passphrase.as_ref().map(|_| "[SET]"))
            .field("strategy_name", &self.strategy_name)
            .field("split_amount", &self.split_amount)
            .field("strategy_order_size", &self.strategy_order_size)
            .field("max_markets", &self.max_markets)
            .finish()
    }
}

impl AppConfig {
    pub fn try_from_env() -> Option<Self> {
        let private_key = std::env::var("PRIVATE_KEY").ok()?;
        let wallet_str = std::env::var("POLYMARKET_WALLET_ADDRESS").ok()?;
        let wallet_address = alloy::primitives::Address::from_str(&wallet_str).ok()?;

        let funder_address = std::env::var("FUNDER_ADDRESS")
            .ok()
            .filter(|v| !v.is_empty())
            .and_then(|v| alloy::primitives::Address::from_str(&v).ok());

        let signature_type = std::env::var("SIGNATURE_TYPE")
            .unwrap_or_else(|_| "0".into())
            .parse::<SignatureType>()
            .ok()?;

        let clob_host = std::env::var("CLOB_HOST")
            .unwrap_or_else(|_| "https://clob.polymarket.com".into());
        let gamma_host = std::env::var("GAMMA_HOST")
            .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into());
        let bridge_host = std::env::var("BRIDGE_HOST")
            .unwrap_or_else(|_| "https://bridge.polymarket.com".into());
        let rpc_url = std::env::var("RPC_URL")
            .unwrap_or_else(|_| "https://polygon-rpc.com".into());
        let relayer_host = std::env::var("RELAYER_HOST")
            .unwrap_or_else(|_| "https://relayer-v2.polymarket.com".into());

        let relayer_api_key = std::env::var("RELAYER_API_KEY").ok()
            .filter(|v| !v.is_empty()).map(|v| SecretString(v));
        let relayer_api_secret = std::env::var("RELAYER_API_SECRET").ok()
            .filter(|v| !v.is_empty()).map(|v| SecretString(v));
        let relayer_passphrase = std::env::var("RELAYER_PASSPHRASE").ok()
            .filter(|v| !v.is_empty()).map(|v| SecretString(v));

        let chain_id = parse_or_default("CHAIN_ID", 137u64).ok()?;
        let dry_run = parse_or_default("DRY_RUN", true).ok()?;
        let enable_test_order = parse_or_default("ENABLE_TEST_ORDER", false).ok()?;

        let max_order_size = parse_decimal("MAX_ORDER_SIZE", Decimal::new(10, 0)).ok()?;
        let max_position_size = parse_decimal("MAX_POSITION_SIZE", Decimal::new(50, 0)).ok()?;
        let order_price_min = parse_decimal("ORDER_PRICE_MIN", Decimal::new(5, 2)).ok()?;
        let order_price_max = parse_decimal("ORDER_PRICE_MAX", Decimal::new(95, 2)).ok()?;

        let strategy_name = std::env::var("STRATEGY").unwrap_or_else(|_| "noop".into());
        let split_amount = parse_or_default("SPLIT_AMOUNT", 10u64).ok()?;
        let strategy_order_size = parse_or_default("STRATEGY_ORDER_SIZE", 5.0f64).ok()?;
        let max_markets = parse_or_default("EXECUTOR_MAX_MARKETS", 0u64).ok()?;

        if order_price_min >= order_price_max
            || order_price_min <= Decimal::ZERO
            || order_price_max >= Decimal::ONE
            || max_order_size <= Decimal::ZERO
        {
            tracing::warn!("Executor config validation failed, disabling executor");
            return None;
        }

        Some(Self {
            private_key: SecretString(private_key),
            wallet_address,
            funder_address,
            signature_type,
            clob_host,
            gamma_host,
            bridge_host,
            rpc_url,
            chain_id,
            dry_run,
            enable_test_order,
            max_order_size,
            max_position_size,
            order_price_min,
            order_price_max,
            relayer_host,
            relayer_api_key,
            relayer_api_secret,
            relayer_passphrase,
            strategy_name,
            split_amount,
            strategy_order_size,
            max_markets,
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        Self::try_from_env().ok_or_else(|| ConfigError::Missing("PRIVATE_KEY or POLYMARKET_WALLET_ADDRESS".into()))
    }
}

fn parse_or_default<T: FromStr>(name: &str, default: T) -> Result<T, ConfigError>
where
    T::Err: fmt::Display,
{
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v.parse::<T>().map_err(|e| ConfigError::InvalidValue {
            name: name.into(),
            msg: e.to_string(),
        }),
        _ => Ok(default),
    }
}

fn parse_decimal(name: &str, default: Decimal) -> Result<Decimal, ConfigError> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => {
            Decimal::from_str(&v).map_err(|e| ConfigError::InvalidValue {
                name: name.into(),
                msg: e.to_string(),
            })
        }
        _ => Ok(default),
    }
}
