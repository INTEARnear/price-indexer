use std::time::Duration;

use cached::proc_macro::cached;
use inindexer::{near_indexer_primitives::types::AccountId, near_utils::FtBalance};
use serde::{Deserialize, Deserializer};

use crate::{get_reqwest_client, network::is_testnet};

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

pub mod dec_format_tuple2 {
    use inindexer::near_utils::FtBalance;
    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &(FtBalance, FtBalance), serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (value.0.to_string(), value.1.to_string()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<(FtBalance, FtBalance), D::Error>
    where
        D: Deserializer<'de>,
    {
        let tuple = <(String, String)>::deserialize(deserializer)?;
        Ok((
            tuple.0.parse::<u128>().map_err(D::Error::custom)?,
            tuple.1.parse::<u128>().map_err(D::Error::custom)?,
        ))
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct TokenBalance {
    #[serde(deserialize_with = "dec_format_or_empty")]
    pub balance: FtBalance,
    pub contract_id: AccountId,
}

fn dec_format_or_empty<'de, D>(deserializer: D) -> Result<FtBalance, D::Error>
where
    D: Deserializer<'de>,
{
    let s = <String>::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(0)
    } else {
        Ok(s.parse::<u128>().map_err(serde::de::Error::custom)?)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AccountTokenBalances {
    #[allow(dead_code)]
    pub account_id: AccountId,
    pub tokens: Vec<TokenBalance>,
}

#[cached(time = 1, result = true)]
pub async fn get_user_token_balances(
    account_id: AccountId,
) -> anyhow::Result<AccountTokenBalances> {
    let client = get_reqwest_client();
    let url = if is_testnet() {
        format!("https://test.api.fastnear.com/v1/account/{account_id}/ft")
    } else {
        format!("https://api.fastnear.com/v1/account/{account_id}/ft")
    };
    let response = client.get(&url).send().await?;
    let balances = response.json::<AccountTokenBalances>().await?;
    Ok(balances)
}
