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
        PoolType::Aidols(pool) => {
            if pool.is_deployed {
                return Some(PoolData {
                    tokens: ("wrap.near".parse().unwrap(), pool.token_id.clone()),
                    ratios: (0.into(), 0.into()),
                    liquidity: (0.into(), 0.into()),
                });
            }

            let amount0 = BigDecimal::from_str(&pool.wnear_hold.to_string()).ok()?;
            let amount1 = BigDecimal::from_str(&pool.token_hold.to_string()).ok()?;

            if amount0 == 0.into() || amount1 == 0.into() {
                return None;
            }
            let token0_in_1_token1 = amount0.clone() / amount1.clone();
            let token1_in_1_token0 = amount1.clone() / amount0.clone();

            Some(PoolData {
                tokens: ("wrap.near".parse().unwrap(), pool.token_id.clone()),
                ratios: (token0_in_1_token1, token1_in_1_token0),
                liquidity: (amount0, amount1),
            })
        }
        PoolType::GraFun(pool) => {
            if pool.is_deployed {
                return Some(PoolData {
                    tokens: ("wrap.near".parse().unwrap(), pool.token_id.clone()),
                    ratios: (0.into(), 0.into()),
                    liquidity: (0.into(), 0.into()),
                });
            }

            let amount0 = BigDecimal::from_str(&pool.wnear_hold.to_string()).ok()?;
            let amount1 = BigDecimal::from_str(&pool.token_hold.to_string()).ok()?;

            if amount0 == 0.into() || amount1 == 0.into() {
                return None;
            }
            let token0_in_1_token1 = amount0.clone() / amount1.clone();
            let token1_in_1_token0 = amount1.clone() / amount0.clone();

            Some(PoolData {
                tokens: ("wrap.near".parse().unwrap(), pool.token_id.clone()),
                ratios: (token0_in_1_token1, token1_in_1_token0),
                liquidity: (amount0, amount1),
            })
        }
        PoolType::Veax(pool) => {
            let mut non_zero_prices: Vec<f64> = pool
                .sqrt_prices
                .iter()
                .filter(|&&price| price != 0.0)
                .copied()
                .collect();

            if non_zero_prices.is_empty() {
                return None;
            }

            // Calculate median, to get the fairest / most common price among all fee tiers
            non_zero_prices.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median_idx = non_zero_prices.len() / 2;
            let median = if non_zero_prices.len() % 2 == 0 {
                (non_zero_prices[median_idx - 1] + non_zero_prices[median_idx]) / 2.0
            } else {
                non_zero_prices[median_idx]
            };

            let token1_in_1_token0 = BigDecimal::from_str(&(median * median).to_string()).ok()?;
            let token0_in_1_token1 =
                BigDecimal::from_str(&(1.0 / (median * median)).to_string()).ok()?;

            Some(PoolData {
                tokens: pool.pool.clone(),
                ratios: (token0_in_1_token1, token1_in_1_token0),
                // in DCL pools, you can't just count all liquidity, since someone can set
                // $10000000000 in very low / very high range and it's effectively not usable
                liquidity: (0.into(), 0.into()),
            })
        }
    }
}
