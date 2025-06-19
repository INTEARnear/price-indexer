//! This module provides functions to get the total supply and circulating supply of a fungible token on NEAR.
//!
//! The total supply is the amount of tokens that have been minted minus the amount that has been burned.
//!
//! The circulating supply is the amount of tokens that are in circulation, which is the total supply minus
//! the amount that has been sent to burn addresses and the amount that is excluded from circulation.
//! The excluded addresses include team addresses (but not individual team members), locked addresses (as long
//! as the tokens are not already unlocked waiting to be claimed), project DAOs and unused supply on other chains.
//! Staking contracts are not included in the excluded addresses, as the tokens can be withdrawn and used, usually
//! without a delay so significant as to call them non-circulating. But even if the tokens will be released to the
//! circulation soon (for example, wallets reserved for liquidity), they are still considered non-circulating, and
//! after they are released, the balance of these wallets will decrease, so the circulating supply will increase.
//!
//! The initial list was made based on top holders of the tokens. If your project is here and something is wrong,
//! please open an issue in this repo or DM t.me/slimytentacles. Also, feel free to add a custom implementation for
//! your smart contract logic, if 1 contract may have both circulating and not-yet-circulating tokens, like staking.

use cached::proc_macro::cached;
use inindexer::near_indexer_primitives::{
    types::{AccountId, Balance, BlockReference, Finality},
    views::QueryRequest,
};
use inindexer::near_utils::dec_format;
use near_jsonrpc_client::{
    errors::JsonRpcError,
    methods::{self, query::RpcQueryError},
    JsonRpcClient,
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use serde::Deserialize;

use crate::{get_reqwest_client, utils::get_rpc_url};

const ZERO_ADDRESS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Completely replaces the total supply
#[cached(time = 3600, option = true)]
async fn hardcoded_total_supply(token_id: AccountId) -> Option<Balance> {
    match token_id.as_str() {
        "aurora" | "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2.factory.bridge.near" => {
            #[derive(Debug, Deserialize)]
            struct ApiResponse {
                #[serde(with = "dec_format")]
                result: Balance,
            }

            let response = get_reqwest_client()
                .get("https://api.etherscan.io/api?module=stats&action=ethsupply")
                .send()
                .await
                .ok()?;
            let response: ApiResponse = response.json().await.ok()?;
            Some(response.result)
        }
        "wrap.near" => {
            #[derive(Debug, Deserialize)]
            struct ApiResponse {
                stats: Vec<Stat>,
            }

            #[derive(Debug, Deserialize)]
            struct Stat {
                #[serde(with = "dec_format")]
                total_supply: Balance,
            }

            let response = get_reqwest_client()
                .get("https://api3.nearblocks.io/v1/stats")
                .send()
                .await
                .ok()?;
            let response: ApiResponse = response.json().await.ok()?;
            Some(
                response
                    .stats
                    .into_iter()
                    .map(|stat| stat.total_supply)
                    .next()?,
            )
        }
        "wrap.testnet" => {
            #[derive(Debug, Deserialize)]
            struct ApiResponse {
                stats: Vec<Stat>,
            }

            #[derive(Debug, Deserialize)]
            struct Stat {
                #[serde(with = "dec_format")]
                total_supply: Balance,
            }

            let response = get_reqwest_client()
                .get("https://api3-testnet.nearblocks.io/v1/stats")
                .send()
                .await
                .ok()?;
            let response: ApiResponse = response.json().await.ok()?;
            Some(
                response
                    .stats
                    .into_iter()
                    .map(|stat| stat.total_supply)
                    .next()?,
            )
        }
        _ => None,
    }
}

/// Replaces the total supply, but also subtracts the amount sent to burn addresses
#[cached(time = 3600)]
async fn hardcoded_max_total_supply(token_id: AccountId) -> Option<Balance> {
    match token_id.as_str() {
        "802d89b6e511b335f05024a65161bce7efc3f311.factory.bridge.near" => {
            Some(1_000_000_000e18 as u128)
        }
        "mpdao-token.near" => Some(1_000_000_000e6 as u128),
        "438e48ed4ce6beecf503d43b9dbd3c30d516e7fd.factory.bridge.near" => {
            Some(10_000_000e18 as u128)
        }
        _ => None,
    }
}

/// Completely replaces the circulating supply
#[cached(time = 3600)]
async fn hardcoded_circulating_supply(token_id: AccountId) -> Option<Balance> {
    match token_id.as_str() {
        "wrap.near" => {
            #[derive(Debug, Deserialize)]
            struct ApiResponse {
                stats: Vec<Stat>,
            }

            #[derive(Debug, Deserialize)]
            struct Stat {
                #[serde(with = "dec_format")]
                circulating_supply: Balance,
            }

            let client = reqwest::Client::new();
            let response = client
                .get("https://api3.nearblocks.io/v1/stats")
                .send()
                .await
                .ok()?;
            let response: ApiResponse = response.json().await.ok()?;
            Some(
                response
                    .stats
                    .into_iter()
                    .map(|stat| stat.circulating_supply)
                    .next()?,
            )
        }
        "wrap.testnet" => {
            #[derive(Debug, Deserialize)]
            struct ApiResponse {
                stats: Vec<Stat>,
            }

            #[derive(Debug, Deserialize)]
            struct Stat {
                #[serde(with = "dec_format")]
                circulating_supply: Balance,
            }

            let response = get_reqwest_client()
                .get("https://api3-testnet.nearblocks.io/v1/stats")
                .send()
                .await
                .ok()?;
            let response: ApiResponse = response.json().await.ok()?;
            Some(
                response
                    .stats
                    .into_iter()
                    .map(|stat| stat.circulating_supply)
                    .next()?,
            )
        }
        _ => None,
    }
}

/// This amount is subtracted from the circulating supply
#[cached(time = 3600)]
async fn hardcoded_additional_non_circulating_supply(token_id: AccountId) -> Option<Balance> {
    match token_id.as_str() {
        "802d89b6e511b335f05024a65161bce7efc3f311.factory.bridge.near" => {
            // https://etherscan.io/address/0x4663f0fd29ba8d14c9a734175e5e9550425c0ba4
            Some(800_000_000e18 as u128)
        }
        "mpdao-token.near" => Some(450_000_000e6 as u128), // https://etherscan.io/address/0x24d9664ba8384d94499d6698ab285b69e879d971
        _ => None,
    }
}

fn burn_addresses(token_id: &AccountId) -> Vec<AccountId> {
    match token_id.as_str() {
        "ft.zomland.near" => vec![
            "burn.zomland.near".parse().unwrap(),
            ZERO_ADDRESS.parse().unwrap(),
        ],
        "token.burrow.near" => vec![
            "blackhole.ref-labs.near".parse().unwrap(),
            ZERO_ADDRESS.parse().unwrap(),
        ],
        _ => vec![ZERO_ADDRESS.parse().unwrap()],
    }
}

/// Locked addresses with tokens that are not in circulation
fn locked_addresses(token_id: &AccountId) -> Vec<AccountId> {
    match token_id.as_str() {
        "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1" => vec![
            "4a432082cc1cd551b14d6f075e9345087d64f57c2660400ef33cac5a0bb263b0"
                .parse()
                .unwrap(), // USDC account that burns tokens for some reason but doesn't mint
        ],
        "usdt.tether-token.near" => vec!["tether-treasury.near".parse().unwrap()],
        "token.burrow.near" => vec!["lockup.burrow.near".parse().unwrap()],
        "token.pumpopoly.near" => vec!["world.pumpopoly.near".parse().unwrap()],
        _ => Vec::new(),
    }
}

/// Team addresses with tokens that are or are not in circulation (depending on who uses it)
fn team_addresses(token_id: &AccountId) -> Vec<AccountId> {
    match token_id.as_str() {
        "mpdao-token.near" => vec![
            "meta-pool-dao.near".parse().unwrap(),
            "meta-pool-dao-2.near".parse().unwrap(),
            "meta-pool-dao-3.near".parse().unwrap(),
            "meta-pool-dao-4.near".parse().unwrap(),
            "meta-pool-dao-5.near".parse().unwrap(),
        ],
        "802d89b6e511b335f05024a65161bce7efc3f311.factory.bridge.near" => {
            vec!["linear.sputnik-dao.near".parse().unwrap()]
        }
        "438e48ed4ce6beecf503d43b9dbd3c30d516e7fd.factory.bridge.near" => {
            vec!["uwondao.near".parse().unwrap()]
        }
        "token.burrow.near" => vec!["burrow.sputnik-dao.near".parse().unwrap()],
        "token.v2.ref-finance.near" => vec![
            "ref-finance.sputnik-dao.near".parse().unwrap(),
            "ref.ref-dev-fund.near".parse().unwrap(),
            "dao.ref-dev-team.near".parse().unwrap(),
            "boostfarm.ref-labs.near".parse().unwrap(),
            "v2.ref-farming.near".parse().unwrap(),
            "ref-farm-reward-proxy.near".parse().unwrap(),
        ],
        "token.paras.near" => vec![
            "community.paras-vesting.near".parse().unwrap(),
            "team.paras.near".parse().unwrap(),
            "reserved.paras.near".parse().unwrap(),
            "team.paras-vesting.near".parse().unwrap(),
            "community.paras.near".parse().unwrap(),
        ],
        "blackdragon.tkn.near" => vec!["black-dragon-community.sputnik-dao.near".parse().unwrap()],
        "nearnvidia.near" => vec!["nearvidia.sputnik-dao.near".parse().unwrap()],
        "usmeme.tg" => vec![
            "usmemedao.near".parse().unwrap(),
            "pad.hot.tg".parse().unwrap(),
            "boxes.hot.tg".parse().unwrap(),
        ],
        "aaaaaa20d9e0e2461697782ef11675f668207961.factory.bridge.near" => vec![
            "auroradao.sputnik-dao.near".parse().unwrap(),
            "auroralabs.sputnik-dao.near".parse().unwrap(),
            "aurorafinance.sputnik-dao.near".parse().unwrap(),
            "aurora-dao-bvi.sputnik-dao.near".parse().unwrap(),
            "aurora-liquidity.near".parse().unwrap(),
            "auroraliquid.sputnik-dao.near".parse().unwrap(),
        ],
        "ftv2.nekotoken.near" => vec![
            "liquidity.nekotoken.near".parse().unwrap(),
            "creators.nekotoken.near".parse().unwrap(),
            "nekotoken.near".parse().unwrap(),
            "marketing.nekotoken.near".parse().unwrap(),
            "treasury.nekotoken.near".parse().unwrap(),
            "coreteam.nekotoken.near".parse().unwrap(),
        ],
        "ft.zomland.near" => vec![
            "treasury.zomland.near".parse().unwrap(),
            "ft.zomland.near".parse().unwrap(),
            "liquidity.zomland.near".parse().unwrap(),
        ],
        "bean.tkn.near" => vec![
            "beanlabs-treasury.near".parse().unwrap(),
            "beanlabs-marketing.near".parse().unwrap(),
            "beanlabs-team.near".parse().unwrap(),
            "beanlabs-farm.near".parse().unwrap(),
            "beanlabs-airdrop.near".parse().unwrap(),
            "beanlabs.near".parse().unwrap(),
        ],
        "intel.tkn.near" => vec![
            "intear.sputnik-dao.near".parse().unwrap(),
            "intear.near".parse().unwrap(),
            "intear-rewards.near".parse().unwrap(),
        ],
        "token.0xshitzu.near" => vec!["shitzu.sputnik-dao.near".parse().unwrap()],
        "dd.tg" => vec![
            "doubledogdao.near".parse().unwrap(),
            "ddpool.near".parse().unwrap(),
        ],
        _ => Vec::new(),
    }
}

// ---
// The hardcoded part ends here
// ---

#[derive(Debug)]
pub enum SupplyError {
    RequestError(#[allow(dead_code)] JsonRpcError<RpcQueryError>),
    ResponseDeserializationError(#[allow(dead_code)] serde_json::Error),
}

pub async fn get_total_supply(token_id: &AccountId) -> Result<Balance, SupplyError> {
    if let Some(hardcoded_total_supply) = hardcoded_total_supply(token_id.clone()).await {
        return Ok(hardcoded_total_supply);
    }
    let total_supply = if let Some(hardcoded_max_total_supply) =
        hardcoded_max_total_supply(token_id.clone()).await
    {
        hardcoded_max_total_supply
    } else {
        get_ft_total_supply(token_id.clone()).await?
    };
    let sent_to_burn_addresses = get_sent_to_burn_addresses(token_id.clone()).await?;
    Ok(total_supply.saturating_sub(sent_to_burn_addresses))
}

pub async fn get_circulating_supply(
    token_id: &AccountId,
    exclude_team: bool,
) -> Result<Balance, SupplyError> {
    if let Some(hardcoded_circulating_supply) = hardcoded_circulating_supply(token_id.clone()).await
    {
        return Ok(hardcoded_circulating_supply);
    }
    let total_supply = get_ft_total_supply(token_id.clone()).await?;
    let sent_to_burn_addresses = get_sent_to_burn_addresses(token_id.clone()).await?;
    let excluded_supply = get_excluded_supply(token_id.clone(), exclude_team).await?;
    let additional_non_circulating_supply =
        hardcoded_additional_non_circulating_supply(token_id.clone())
            .await
            .unwrap_or_default();
    Ok(total_supply
        .saturating_sub(sent_to_burn_addresses)
        .saturating_sub(excluded_supply)
        .saturating_sub(additional_non_circulating_supply))
}

// TODO refresh on mint and burn events
#[cached(time = 3600, result = true)]
pub async fn get_ft_total_supply(token_id: AccountId) -> Result<Balance, SupplyError> {
    if let Some(hardcoded_max_total_supply) = hardcoded_max_total_supply(token_id.clone()).await {
        return Ok(hardcoded_max_total_supply);
    }
    let client = JsonRpcClient::connect(get_rpc_url());
    let request = methods::query::RpcQueryRequest {
        block_reference: BlockReference::Finality(Finality::None),
        request: QueryRequest::CallFunction {
            account_id: token_id,
            method_name: "ft_total_supply".into(),
            args: Vec::new().into(),
        },
    };
    let response = client
        .call(request)
        .await
        .map_err(SupplyError::RequestError)?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    Ok(
        serde_json::from_slice::<StringifiedBalance>(&call_result.result)
            .map_err(SupplyError::ResponseDeserializationError)?
            .0,
    )
}

#[cached(time = 3600, result = true)]
pub async fn get_sent_to_burn_addresses(token_id: AccountId) -> Result<Balance, SupplyError> {
    let mut total_sent_to_burn_addresses = 0;
    for burn_address in burn_addresses(&token_id) {
        let balance = get_balance(token_id.clone(), burn_address.clone()).await?;
        total_sent_to_burn_addresses += balance;
    }
    Ok(total_sent_to_burn_addresses)
}

#[cached(time = 3600, result = true)]
pub async fn get_excluded_supply(
    token_id: AccountId,
    exclude_team: bool,
) -> Result<Balance, SupplyError> {
    let mut total_excluded_supply = 0;
    for excluded_address in locked_addresses(&token_id) {
        let balance = get_balance(token_id.clone(), excluded_address.clone()).await?;
        total_excluded_supply += balance;
    }
    if exclude_team {
        for team_address in team_addresses(&token_id) {
            let balance = get_balance(token_id.clone(), team_address.clone()).await?;
            total_excluded_supply += balance;
        }
    }
    Ok(total_excluded_supply)
}

#[cached(time = 60, result = true)]
async fn get_balance(token_id: AccountId, account_id: AccountId) -> Result<Balance, SupplyError> {
    let client = JsonRpcClient::connect(get_rpc_url());
    let request = methods::query::RpcQueryRequest {
        block_reference: BlockReference::Finality(Finality::None),
        request: QueryRequest::CallFunction {
            account_id: token_id,
            method_name: "ft_balance_of".into(),
            args: serde_json::to_vec(&serde_json::json!({
                "account_id": account_id
            }))
            .unwrap()
            .into(),
        },
    };
    let response = client
        .call(request)
        .await
        .map_err(SupplyError::RequestError)?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    Ok(
        serde_json::from_slice::<StringifiedBalance>(&call_result.result)
            .map_err(SupplyError::ResponseDeserializationError)?
            .0,
    )
}

#[derive(Debug, Deserialize)]
struct StringifiedBalance(#[serde(with = "dec_format")] Balance);
