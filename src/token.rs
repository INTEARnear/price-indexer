use std::collections::HashMap;
use std::str::FromStr;

use inindexer::near_indexer_primitives::types::AccountId;
use intear_events::events::trade::trade_pool_change::PoolType;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::pool_data::PoolData;
use crate::token_metadata::TokenMetadata;
use crate::tokens::USD_TOKEN;
use crate::utils::serde_bigdecimal;

type GetTokenPriceFn = fn(&BigDecimal) -> BigDecimal;
const HARDCODED_TOKEN_PRICES: &[(&str, GetTokenPriceFn)] = &[
    // ("usdt.tether-token.near", stablecoin_price), // USDt is already always 1.00
    (
        "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1", // USDC
        stablecoin_price,
    ),
    (
        "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near", // USDT.e
        stablecoin_price,
    ),
    (
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near", // USDC.e
        stablecoin_price,
    ),
    (
        "853d955acef822db058eb8505911ed77f175b99e.factory.bridge.near", // FRAX
        stablecoin_price,
    ),
    ("pre.meteor-token.near", |_| 0.into()), // MEPT
                                             // TODO fetch HOT futures price from WhiteBIT
];

fn stablecoin_price(actual_price_usd: &BigDecimal) -> BigDecimal {
    const STABLECOIN_BASE_PRICE_USD: f64 = 1.0;
    const STABLECOIN_MAX_DIFFERENCE_USD: f64 = 0.01;

    let price = actual_price_usd.clone();
    let price_f64 = f64::from_str(&price.to_string()).unwrap();
    if (price_f64 - STABLECOIN_BASE_PRICE_USD).abs() < STABLECOIN_MAX_DIFFERENCE_USD {
        1.into()
    } else {
        price
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd_raw: BigDecimal,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd: BigDecimal,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd_hardcoded: BigDecimal,
    /// 'Main pool' is a pool that leads to a token that can be farther converted
    /// into [`USD_TOKEN`] through one of [`USD_ROUTES`].
    pub main_pool: Option<String>,
    pub metadata: TokenMetadata,
}

pub fn calculate_price(
    token_id: &AccountId,
    pools: &HashMap<String, (PoolType, PoolData)>,
    routes: &HashMap<AccountId, String>,
    pool: &str,
) -> BigDecimal {
    if token_id == USD_TOKEN {
        return BigDecimal::from(1);
    }

    let mut current_token_id = token_id.clone();
    let mut current_pool_data = &pools.get(pool).unwrap().1;
    let mut current_amount = BigDecimal::from(1);
    (current_token_id, current_amount) = if current_pool_data.tokens.0 == current_token_id {
        (
            current_pool_data.tokens.1.clone(),
            current_amount * current_pool_data.ratios.1.clone(),
        )
    } else {
        (
            current_pool_data.tokens.0.clone(),
            current_amount * current_pool_data.ratios.0.clone(),
        )
    };
    loop {
        if current_token_id == USD_TOKEN {
            break current_amount;
        }
        if let Some(next_pool) = routes.get(&current_token_id) {
            current_pool_data = if let Some((_pool, data)) = pools.get(next_pool) {
                data
            } else {
                break BigDecimal::from(0);
            };
            (current_token_id, current_amount) = if current_pool_data.tokens.0 == current_token_id {
                (
                    current_pool_data.tokens.1.clone(),
                    current_amount * current_pool_data.ratios.1.clone(),
                )
            } else {
                (
                    current_pool_data.tokens.0.clone(),
                    current_amount * current_pool_data.ratios.0.clone(),
                )
            };
        } else {
            log::error!("USD route not found for {current_token_id} (for {token_id})");
            break BigDecimal::from(0);
        }
    }
}

pub fn get_hardcoded_price_usd(token_id: &AccountId, actual_price_usd: &BigDecimal) -> BigDecimal {
    if let Some(hardcoded_price_fn) = HARDCODED_TOKEN_PRICES
        .iter()
        .find(|(hardcoded_token_id, _)| hardcoded_token_id == token_id)
        .map(|(_, price_fn)| price_fn)
    {
        hardcoded_price_fn(actual_price_usd)
    } else {
        actual_price_usd.clone()
    }
}
