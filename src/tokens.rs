use std::{collections::HashMap, str::FromStr};

use inindexer::near_indexer_primitives::types::AccountId;
use intear_events::events::trade::trade_pool_change::PoolType;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::{
    pool_data::PoolData,
    supply::{get_circulating_supply, get_total_supply},
    token::{calculate_price, get_hardcoded_price_usd, Token, TokenScore},
    token_metadata::get_token_metadata,
};

/// Used to ignore warnings for this token.
const KNOWN_TOKENS_WITH_NO_POOL: &[&str] = &[];

pub const USD_TOKEN: &str = "usdt.tether-token.near";
pub const USD_DECIMALS: u32 = 6;

// Feel free to add other tokens in ROUTES if you're sure that the pools
// won't unexpectedly go to 0 without this change in the code. If the
// pool here is going to be 0, change it before removing liquidity.
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
        "REF-535",        // STNEAR-NEAR
    ),
    (
        "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near", // USDT.e
        "REF-4",                                                        // NEAR-USDT.e
    ),
    (
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near", // USDC.e
        "REF-3",                                                        // NEAR-USDC.e
    ),
    (
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near", // USDC.e
        "REF-3",                                                        // NEAR-USDC.e
    ),
    (
        "nkok.tkn.near", // nKOK
        "REF-4820",      // nKOK-NEAR
    ),
    (
        "avb.tkn.near", // AVB
        "REF-20",       // AVB-NEAR
    ),
];

#[derive(Debug, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(skip, default = "create_routes_to_usd")]
    pub routes_to_usd: HashMap<AccountId, String>,

    pub tokens: HashMap<AccountId, Token>,
    pub pools: HashMap<String, (PoolType, PoolData)>,
}

fn create_routes_to_usd() -> HashMap<AccountId, String> {
    USD_ROUTES
        .iter()
        .map(|(token_id, pool_id)| (token_id.parse().unwrap(), pool_id.to_string()))
        .collect()
}

impl Tokens {
    pub fn new() -> Self {
        Self {
            routes_to_usd: create_routes_to_usd(),
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
                    && (self.routes_to_usd.contains_key(&pool_data.tokens.1)
                        || pool_data.tokens.1 == USD_TOKEN)
                {
                    max_liquidity = liquidity;
                    max_pool = Some(pool_id.clone());
                }
            } else if pool_data.tokens.1 == *token_id
                && (self.routes_to_usd.contains_key(&pool_data.tokens.0)
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
                            account_id: token_id.clone(),
                            price_usd_raw: BigDecimal::from(0),
                            price_usd: BigDecimal::from(0),
                            price_usd_hardcoded: BigDecimal::from(0),
                            main_pool: None,
                            metadata,
                            total_supply: get_total_supply(&token_id).await.unwrap_or_default(),
                            circulating_supply: get_circulating_supply(&token_id, false)
                                .await
                                .unwrap_or_default(),
                            circulating_supply_excluding_team: get_circulating_supply(
                                &token_id, true,
                            )
                            .await
                            .unwrap_or_default(),
                            reputation: Default::default(),
                            socials: Default::default(),
                            slug: Default::default(),
                        },
                    );
                    self.tokens.get_mut(&token_id).unwrap()
                } else {
                    log::warn!("Couldn't get metadata for {token_id}");
                    continue;
                };

                token.price_usd_raw =
                    calculate_price(&token_id, &self.pools, &self.routes_to_usd, &pool);
                token.price_usd = token.price_usd_raw.clone()
                    * BigDecimal::from_str(&(10u128.pow(token.metadata.decimals)).to_string())
                        .unwrap()
                    / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap();
                token.price_usd_hardcoded = get_hardcoded_price_usd(&token_id, &token.price_usd);
                token.main_pool = Some(pool);
            } else if let Some(token) = self.tokens.get_mut(&token_id) {
                token.main_pool = None;
            } else if let Ok(metadata) = get_token_metadata(token_id.clone()).await {
                self.tokens.insert(
                    token_id.clone(),
                    Token {
                        account_id: token_id.clone(),
                        price_usd_raw: BigDecimal::from(0),
                        price_usd: BigDecimal::from(0),
                        price_usd_hardcoded: BigDecimal::from(0),
                        main_pool: None,
                        metadata,
                        total_supply: get_total_supply(&token_id).await.unwrap_or_default(),
                        circulating_supply: get_circulating_supply(&token_id, false)
                            .await
                            .unwrap_or_default(),
                        circulating_supply_excluding_team: get_circulating_supply(&token_id, true)
                            .await
                            .unwrap_or_default(),
                        reputation: Default::default(),
                        socials: Default::default(),
                        slug: Default::default(),
                    },
                );
            } else {
                log::warn!("Couldn't get metadata for {token_id}");
            }
        }
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
                token.price_usd_raw =
                    calculate_price(&token_id, &self.pools, &self.routes_to_usd, main_pool);
                token.price_usd = token.price_usd_raw.clone()
                    * BigDecimal::from_str(&(10u128.pow(token.metadata.decimals)).to_string())
                        .unwrap()
                    / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap();
                token.price_usd_hardcoded = get_hardcoded_price_usd(&token_id, &token.price_usd);
            }
        }
    }

    pub fn search_tokens(
        &self,
        search: &str,
        take: usize,
        min_reputation: TokenScore,
    ) -> Vec<&Token> {
        self.tokens
            .values()
            .filter(|token| token.reputation >= min_reputation)
            .map(|token| (token, token.sorting_score(search)))
            .filter(|(_, score)| *score > 0)
            .sorted_by_key(|(_, score)| -(*score as i32))
            .map(|(token, _)| token)
            .take(take)
            .collect()
    }
}
