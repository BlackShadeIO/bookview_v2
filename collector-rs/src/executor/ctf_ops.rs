use alloy::primitives::{Address, Bytes, B256, U256, address};
use alloy::sol_types::SolCall;
use anyhow::Result;
use rust_decimal_macros::dec;

use super::auth::AuthenticatedClobClient;
use super::config::AppConfig;
use super::relayer::RelayerClient;

pub const USDC_E: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
pub const PUSD: Address = address!("0xc011a7e12a19f7b1f670d46f03b03f3342e82dfb");
pub const CTF_ADAPTER: Address = address!("0xAdA100Db00Ca00073811820692005400218FcE1f");

alloy::sol! {
    interface IConditionalTokens {
        function splitPosition(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata partition,
            uint256 amount
        ) external;

        function mergePositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata partition,
            uint256 amount
        ) external;

        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;
    }

    interface IERC1155 {
        function setApprovalForAll(address operator, bool approved) external;
    }

    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

pub async fn split_position(
    http: &reqwest::Client,
    config: &AppConfig,
    condition_id: &str,
    amount: u64,
) -> Result<String> {
    let signer = super::auth::create_signer(config)?;
    let relayer = RelayerClient::new(config, signer, http.clone())?;

    let condition_id: B256 = condition_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid condition ID hex"))?;
    let amount_units = U256::from(amount) * U256::from(1_000_000u64);

    let approve_data = IERC20::approveCall {
        spender: CTF_ADAPTER,
        amount: U256::MAX,
    }
    .abi_encode();

    let split_data = IConditionalTokens::splitPositionCall {
        collateralToken: USDC_E,
        parentCollectionId: B256::ZERO,
        conditionId: condition_id,
        partition: vec![U256::from(1), U256::from(2)],
        amount: amount_units,
    }
    .abi_encode();

    let calls = vec![
        super::relayer::ProxyCall {
            typeCode: 1,
            to: PUSD,
            value: U256::ZERO,
            data: Bytes::from(approve_data),
        },
        super::relayer::ProxyCall {
            typeCode: 1,
            to: CTF_ADAPTER,
            value: U256::ZERO,
            data: Bytes::from(split_data),
        },
    ];

    let result = relayer.execute_and_wait(calls, "strategy split").await?;
    Ok(result.transaction_hash.unwrap_or_else(|| "pending".into()))
}

pub async fn merge_positions(
    http: &reqwest::Client,
    config: &AppConfig,
    condition_id: &str,
    amount: u64,
) -> Result<String> {
    let signer = super::auth::create_signer(config)?;
    let relayer = RelayerClient::new(config, signer, http.clone())?;

    let contract_cfg = polymarket_client_sdk_v2::contract_config(config.chain_id, false)
        .ok_or_else(|| anyhow::anyhow!("No contract config for chain {}", config.chain_id))?;

    let condition_id: B256 = condition_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid condition ID hex"))?;
    let ctf_address = contract_cfg.conditional_tokens;
    let amount_units = U256::from(amount) * U256::from(1_000_000u64);

    let approve_ctf = IERC1155::setApprovalForAllCall {
        operator: CTF_ADAPTER,
        approved: true,
    }
    .abi_encode();

    let merge_data = IConditionalTokens::mergePositionsCall {
        collateralToken: USDC_E,
        parentCollectionId: B256::ZERO,
        conditionId: condition_id,
        partition: vec![U256::from(1), U256::from(2)],
        amount: amount_units,
    }
    .abi_encode();

    let calls = vec![
        super::relayer::ProxyCall {
            typeCode: 1,
            to: ctf_address,
            value: U256::ZERO,
            data: Bytes::from(approve_ctf),
        },
        super::relayer::ProxyCall {
            typeCode: 1,
            to: CTF_ADAPTER,
            value: U256::ZERO,
            data: Bytes::from(merge_data),
        },
    ];

    let result = relayer.execute_and_wait(calls, "merge positions").await?;
    Ok(result.transaction_hash.unwrap_or_else(|| "pending".into()))
}

pub async fn register_positions(
    clob_client: &AuthenticatedClobClient,
    signer: &alloy::signers::local::PrivateKeySigner,
    clob_token_ids: &[String],
) -> Result<()> {
    for token_id_str in clob_token_ids {
        let token_id: U256 = token_id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid token ID"))?;

        let update_req =
            polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest::builder()
                .asset_type(polymarket_client_sdk_v2::clob::types::AssetType::Conditional)
                .token_id(token_id)
                .build();

        clob_client
            .update_balance_allowance(update_req)
            .await
            .map_err(|e| anyhow::anyhow!("balance_allowance failed for {token_id_str}: {e}"))?;

        tracing::info!(token_id = %token_id_str, "Balance allowance updated");
    }

    // Place-then-cancel trick to register position with CLOB
    let yes_token = clob_token_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("no YES token ID"))?;

    let resp = super::clob::place_limit_order(
        clob_client,
        signer,
        super::clob::LimitOrderParams {
            token_id: yes_token.clone(),
            side: super::clob::Side::Sell,
            price: dec!(0.95),
            size: dec!(5.0),
        },
    )
    .await?;

    let _ = super::clob::cancel_order(clob_client, &resp.order_id).await;
    tracing::info!("Position registered with CLOB via place+cancel");
    Ok(())
}
