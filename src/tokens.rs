use std::{collections::HashMap, str::FromStr};

use inindexer::near_indexer_primitives::types::AccountId;
use intear_events::events::trade::trade_pool_change::PoolType;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::{
    pool_data::PoolData,
    token_metadata::{get_token_metadata, TokenMetadata},
    utils::serde_bigdecimal,
};

/// Used to ignore warnings for this token.
const KNOWN_TOKENS_WITH_NO_POOL: &[&str] = &[];

// Feel free to add other tokens in ROUTES if you're sure that the pools
// won't unexpectedly go to 0 without this change in the code. If the
// pool here is going to be 0, change it before removing liquidity.
pub const USD_TOKEN: &str = "usdt.tether-token.near";
const USD_ROUTES: &[(&str, &str)] = &[
    ("wrap.near", "REF-3879"), // NEAR-USDt
    // TODO FRAX when stableswap is implemented, but no one uses it anyway so not a priority
    // (
    //     "853d955acef822db058eb8505911ed77f175b99e.factory.bridge.near", // FRAX
    //     "4514", // FRAX-USDC stableswap
    // ),
    (
        "blackdragon.tkn.near", // BLACKDRAGON
        "REF-4276",             // BLACKDRAGON-NEAR
    ),
    (
        "intel.tkn.near", // INTEAR
        "REF-4663",       // INTEL-NEAR
    ),
    (
        "ftv2.nekotoken.near", // NEKO
        "REF-3807",            // NEKO-NEAR
    ),
    (
        "meta-pool.near", // Staked NEAR
        "REF-1923",       // STNEAR-NEAR
    ),
    (
        "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near", // USDT.e
        "REF-4",                                                        // NEAR-USDT.e
    ),
    (
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near", // USDC.e
        "REF-3",                                                        // NEAR-USDC.e
    ),
];

#[derive(Debug, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(skip, default = "create_hardcoded_usd_routes")]
    pub hardcoded_usd_routes: HashMap<AccountId, String>,

    pub tokens: HashMap<AccountId, Token>,
    pub pools: HashMap<String, (PoolType, PoolData)>,
}

fn create_hardcoded_usd_routes() -> HashMap<AccountId, String> {
    USD_ROUTES
        .iter()
        .map(|(token_id, pool_id)| (token_id.parse().unwrap(), pool_id.to_string()))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(with = "serde_bigdecimal")]
    pub price_usd: BigDecimal,
    /// 'Main pool' is a pool that leads to a token that can be farther converted
    /// into [`USD_TOKEN`] through one of [`USD_ROUTES`].
    pub main_pool: Option<String>,
    pub metadata: TokenMetadata,
}

impl Tokens {
    pub fn new() -> Self {
        Self {
            hardcoded_usd_routes: create_hardcoded_usd_routes(),
            tokens: HashMap::new(),
            pools: HashMap::new(),
        }
    }

    pub fn recalculate_token(&self, token_id: &AccountId) -> Option<String> {
        let mut max_liquidity = 0.into();
        let mut max_pool = None;
        for (pool_id, (_pool, pool_data)) in &self.pools {
            if pool_data.tokens.0 == *token_id {
                let liquidity = pool_data.liquidity.0.clone();
                if liquidity > max_liquidity
                    && (self.hardcoded_usd_routes.contains_key(&pool_data.tokens.1)
                        || pool_data.tokens.1 == USD_TOKEN)
                {
                    max_liquidity = liquidity;
                    max_pool = Some(pool_id.clone());
                }
            } else if pool_data.tokens.1 == *token_id
                && (self.hardcoded_usd_routes.contains_key(&pool_data.tokens.0)
                    || pool_data.tokens.0 == USD_TOKEN)
            {
                let liquidity = pool_data.liquidity.1.clone();
                if liquidity > max_liquidity {
                    max_liquidity = liquidity;
                    max_pool = Some(pool_id.clone());
                }
            }
        }
        if max_pool.is_none() && !KNOWN_TOKENS_WITH_NO_POOL.contains(&token_id.as_str()) {
            log::warn!("Can't calculate main pool for {token_id}");
        }
        max_pool
    }

    pub async fn update_pool(&mut self, pool_id: &str, pool: PoolType, data: PoolData) {
        let tokens = [data.tokens.0.clone(), data.tokens.1.clone()];
        self.pools.insert(pool_id.to_string(), (pool, data));
        for token_id in tokens {
            if let Some(pool) = self.recalculate_token(&token_id) {
                let token = if let Some(token) = self.tokens.get_mut(&token_id) {
                    token
                } else if let Ok(metadata) = get_token_metadata(token_id.clone()).await {
                    self.tokens.insert(
                        token_id.clone(),
                        Token {
                            price_usd: BigDecimal::from(0),
                            main_pool: None,
                            metadata,
                        },
                    );
                    self.tokens.get_mut(&token_id).unwrap()
                } else {
                    log::warn!("Couldn't get metadata for {token_id}");
                    continue;
                };

                token.price_usd = calculate_price(
                    token_id.clone(),
                    USD_TOKEN.parse().unwrap(),
                    &self.pools,
                    &self.hardcoded_usd_routes,
                    &pool,
                );
                token.main_pool = Some(pool);
            } else if let Some(token) = self.tokens.get_mut(&token_id) {
                token.main_pool = None;
            } else if let Ok(metadata) = get_token_metadata(token_id.clone()).await {
                self.tokens.insert(
                    token_id.clone(),
                    Token {
                        price_usd: BigDecimal::from(0),
                        main_pool: None,
                        metadata,
                    },
                );
            } else {
                log::warn!("Couldn't get metadata for {token_id}");
            }
        }
    }

    pub fn get_price(&self, token_id: &AccountId) -> Option<BigDecimal> {
        let token = self.tokens.get(token_id)?;
        Some(if token_id == USD_TOKEN {
            1.into()
        } else {
            token.price_usd.clone()
        })
    }

    pub fn recalculate_prices(&mut self) {
        let sorting_order = USD_ROUTES
            .iter()
            .enumerate()
            .map(|(i, (token_id, _))| (AccountId::from_str(token_id).unwrap(), i))
            .collect::<HashMap<_, _>>();
        for token_id in self
            .tokens
            .keys()
            .cloned()
            .sorted_by_key(|token_id| sorting_order.get(token_id).unwrap_or(&usize::MAX))
        {
            let token = self.tokens.get_mut(&token_id).unwrap();
            if let Some(main_pool) = &token.main_pool {
                token.price_usd = calculate_price(
                    token_id.clone(),
                    USD_TOKEN.parse().unwrap(),
                    &self.pools,
                    &self.hardcoded_usd_routes,
                    main_pool,
                );
            }
        }
    }
}

pub fn calculate_price(
    token_id: AccountId,
    target_token: AccountId,
    pools: &HashMap<String, (PoolType, PoolData)>,
    routes: &HashMap<AccountId, String>,
    pool: &str,
) -> BigDecimal {
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
        if current_token_id == target_token {
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
            log::error!("USD route not found for {current_token_id}");
            break BigDecimal::from(0);
        }
    }
}
