use std::str::FromStr;

use crate::utils::serde_bigdecimal_tuple2;
use inindexer::near_indexer_primitives::types::AccountId;
use intear_events::events::trade::trade_pool_change::{PoolType, RefPool};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PoolData {
    pub tokens: (AccountId, AccountId),
    #[serde(with = "serde_bigdecimal_tuple2")]
    pub ratios: (BigDecimal, BigDecimal),
    #[serde(with = "serde_bigdecimal_tuple2")]
    pub liquidity: (BigDecimal, BigDecimal),
}

pub fn extract_pool_data(pool: &PoolType) -> Option<PoolData> {
    match pool {
        PoolType::Ref(pool) => match pool {
            RefPool::SimplePool(pool) => {
                if let (Ok([amount0, amount1]), Ok([token0, token1])) = (
                    <[u128; 2]>::try_from(pool.amounts.clone()),
                    <[AccountId; 2]>::try_from(pool.token_account_ids.clone()),
                ) {
                    let amount0 = BigDecimal::from_str(&amount0.to_string()).ok()?;
                    let amount1 = BigDecimal::from_str(&amount1.to_string()).ok()?;

                    if amount0 == 0.into() || amount1 == 0.into() {
                        return None;
                    }
                    let token0_in_1_token1 = amount0.clone() / amount1.clone();
                    let token1_in_1_token0 = amount1.clone() / amount0.clone();
                    Some(PoolData {
                        tokens: (token0, token1),
                        ratios: (token0_in_1_token1, token1_in_1_token0),
                        liquidity: (amount0, amount1),
                    })
                } else {
                    None
                }
            }
            _ => None,
        },
    }
}
