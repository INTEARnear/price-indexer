use cached::proc_macro::cached;
use inindexer::near_indexer_primitives::types::AccountId;
use serde::{Deserialize, Serialize};

use crate::get_reqwest_client;

pub fn get_rpc_url() -> String {
    std::env::var("RPC_URL").unwrap_or_else(|_| "https://rpc.shitzuapes.xyz".to_string())
}

pub mod serde_bigdecimal {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    use sqlx::types::BigDecimal;
    use std::str::FromStr;

    pub fn serialize<S>(value: &BigDecimal, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BigDecimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BigDecimal::from_str(&s).map_err(D::Error::custom)
    }
}

pub mod serde_bigdecimal_tuple2 {
    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};
    use sqlx::types::BigDecimal;
    use std::str::FromStr;

    pub fn serialize<S>(value: &(BigDecimal, BigDecimal), serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (value.0.to_string(), value.1.to_string()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<(BigDecimal, BigDecimal), D::Error>
    where
        D: Deserializer<'de>,
    {
        let tuple = <(String, String)>::deserialize(deserializer)?;
        Ok((
            BigDecimal::from_str(&tuple.0).map_err(D::Error::custom)?,
            BigDecimal::from_str(&tuple.1).map_err(D::Error::custom)?,
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SupportedTokensRequest {
    pub chains: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcSupportedTokensRequest {
    pub id: i32,
    pub jsonrpc: String,
    pub method: String,
    pub params: Vec<SupportedTokensRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcSupportedTokensResponse {
    pub result: SupportedTokensResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SupportedTokensResponse {
    pub tokens: Vec<NearIntentsTokenInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NearIntentsTokenInfo {
    pub defuse_asset_identifier: String,
    pub near_token_id: String,
    pub decimals: u8,
    pub asset_name: String,
    pub min_deposit_amount: String,
    pub min_withdrawal_amount: String,
    pub withdrawal_fee: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenBalance {
    pub balance: String,
    pub contract_id: AccountId,
    #[serde(rename = "last_update_block_height")]
    pub last_update_block_height: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountTokenBalances {
    pub account_id: AccountId,
    pub tokens: Vec<TokenBalance>,
}

#[cached(time = 30, result = true)]
pub async fn get_user_token_balances(
    account_id: AccountId,
) -> anyhow::Result<AccountTokenBalances> {
    let client = get_reqwest_client();
    let url = format!("https://api.fastnear.com/v1/account/{account_id}/ft");
    let response = client.get(&url).send().await?;
    let balances = response.json::<AccountTokenBalances>().await?;
    Ok(balances)
}
