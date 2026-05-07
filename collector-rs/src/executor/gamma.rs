use chrono::{DateTime, Utc};
use serde::Deserialize;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GammaError {
    #[error("gamma request failed: {0}")]
    RequestFailed(String),
    #[error("gamma response parse error: {0}")]
    ParseError(String),
}

#[derive(Debug, Clone)]
pub struct BtcFiveMinMarket {
    pub market_id: String,
    pub question: String,
    pub slug: String,
    pub condition_id: String,
    pub clob_token_ids: Vec<String>,
    pub outcomes: Vec<String>,
    pub start_date: DateTime<Utc>,
    pub end_date: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct GammaEvent {
    #[serde(default)]
    _slug: Option<String>,
    #[serde(default)]
    markets: Option<Vec<GammaMarket>>,
}

#[derive(Debug, Deserialize)]
struct GammaMarket {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(rename = "conditionId", default)]
    condition_id: Option<String>,
    #[serde(rename = "clobTokenIds", default)]
    clob_token_ids: Option<serde_json::Value>,
    #[serde(default)]
    outcomes: Option<serde_json::Value>,
    #[serde(rename = "startDate", default)]
    start_date: Option<String>,
    #[serde(rename = "endDate", default)]
    end_date: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    closed: Option<bool>,
}

fn parse_token_ids(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_json::Value::String(s) => {
            serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
        }
        _ => vec![],
    }
}

fn parse_outcomes(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_json::Value::String(s) => {
            serde_json::from_str::<Vec<String>>(s).unwrap_or_else(|_| vec![s.clone()])
        }
        _ => vec![],
    }
}

const WINDOW_SECS: i64 = 300;

fn current_window_epoch(now_epoch: i64) -> i64 {
    now_epoch - (now_epoch % WINDOW_SECS)
}

fn slug_for_epoch(epoch: i64) -> String {
    format!("btc-updown-5m-{epoch}")
}

async fn fetch_event_by_slug(
    http: &reqwest::Client,
    gamma_host: &str,
    slug: &str,
) -> Result<Option<GammaEvent>, GammaError> {
    let url = format!("{gamma_host}/events");
    let resp = http
        .get(&url)
        .query(&[("slug", slug)])
        .send()
        .await
        .map_err(|e| GammaError::RequestFailed(e.to_string()))?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let events: Vec<GammaEvent> = resp.json().await.unwrap_or_default();
    Ok(events.into_iter().next())
}

fn extract_market(event: &GammaEvent, now: DateTime<Utc>) -> Option<BtcFiveMinMarket> {
    let markets = event.markets.as_ref()?;
    for market in markets {
        let question = market.question.as_deref()?;
        let slug = market.slug.as_deref().unwrap_or("");

        if market.closed.unwrap_or(false) || !market.active.unwrap_or(true) {
            continue;
        }

        let condition_id = market.condition_id.as_ref()?;
        let clob_token_ids = market.clob_token_ids.as_ref().map(parse_token_ids)?;
        if clob_token_ids.is_empty() {
            continue;
        }

        let end_date = market.end_date.as_deref()?.parse::<DateTime<Utc>>().ok()?;
        if end_date <= now {
            continue;
        }

        let start_date = market
            .start_date
            .as_deref()
            .and_then(|d| d.parse::<DateTime<Utc>>().ok())
            .unwrap_or(now);

        let outcomes = market
            .outcomes
            .as_ref()
            .map(parse_outcomes)
            .unwrap_or_else(|| vec!["Up".into(), "Down".into()]);

        let market_id = match &market.id {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => condition_id.clone(),
        };

        return Some(BtcFiveMinMarket {
            market_id,
            question: question.to_string(),
            slug: slug.to_string(),
            condition_id: condition_id.clone(),
            clob_token_ids,
            outcomes,
            start_date,
            end_date,
        });
    }
    None
}

pub async fn discover_active_btc_5m(
    http: &reqwest::Client,
    gamma_host: &str,
) -> Result<Option<BtcFiveMinMarket>, GammaError> {
    let now = Utc::now();
    let now_epoch = now.timestamp();
    let current_epoch = current_window_epoch(now_epoch);

    for offset in [0, WINDOW_SECS] {
        let epoch = current_epoch + offset;
        let slug = slug_for_epoch(epoch);
        tracing::info!(slug = %slug, "Looking up BTC 5-min market");

        if let Some(event) = fetch_event_by_slug(http, gamma_host, &slug).await? {
            if let Some(market) = extract_market(&event, now) {
                tracing::info!(
                    question = %market.question,
                    condition_id = %market.condition_id,
                    end_date = %market.end_date,
                    tokens = market.clob_token_ids.len(),
                    "Found BTC 5-min market"
                );
                return Ok(Some(market));
            }
        }
    }

    tracing::warn!("No active BTC 5-min market found for current or next window");
    Ok(None)
}
