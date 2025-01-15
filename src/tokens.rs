use std::cmp::Reverse;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::{Duration, Instant},
};

use cached::proc_macro::{cached, io_cached};
use inindexer::near_indexer_primitives::types::{AccountId, Balance, BlockHeight};
use inindexer::near_utils::dec_format;
use intear_events::events::trade::trade_pool_change::PoolType;
use itertools::Itertools;
use num_traits::ToPrimitive;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::{
    get_reqwest_client,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tokens {
    #[serde(skip, default = "create_routes_to_usd")]
    pub routes_to_usd: HashMap<AccountId, String>,

    pub tokens: HashMap<AccountId, Token>,
    pub pools: HashMap<String, (PoolType, PoolData)>,
    #[serde(default)]
    pub spam_tokens: HashSet<AccountId>,
    #[serde(skip)]
    last_checked_metadata: HashMap<AccountId, Instant>,
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
            spam_tokens: HashSet::new(),
            last_checked_metadata: HashMap::new(),
        }
    }

    pub fn recalculate_token(&self, token_id: &AccountId) -> (Option<String>, BigDecimal) {
        let mut max_liquidity = 0.into();
        let mut total_liquidity = 0.into();
        let mut max_pool = None;
        for (pool_id, (_pool, pool_data)) in &self.pools {
            if pool_data.tokens.0 == *token_id {
                let liquidity = pool_data.liquidity.0.clone();
                if liquidity > max_liquidity
                    && (self.routes_to_usd.contains_key(&pool_data.tokens.1)
                        || pool_data.tokens.1 == USD_TOKEN)
                {
                    max_liquidity = liquidity.clone();
                    max_pool = Some(pool_id.clone());
                }
                total_liquidity += liquidity;
            } else if pool_data.tokens.1 == *token_id
                && (self.routes_to_usd.contains_key(&pool_data.tokens.0)
                    || pool_data.tokens.0 == USD_TOKEN)
            {
                let liquidity = pool_data.liquidity.1.clone();
                if liquidity > max_liquidity {
                    max_liquidity = liquidity.clone();
                    max_pool = Some(pool_id.clone());
                }
                total_liquidity += liquidity;
            }
        }
        if max_pool.is_none() && !KNOWN_TOKENS_WITH_NO_POOL.contains(&token_id.as_str()) {
            log::warn!("Can't calculate main pool for {token_id}");
        }
        (max_pool, total_liquidity)
    }

    pub async fn add_token(&mut self, token_id: &AccountId, force: bool) -> bool {
        if self.tokens.contains_key(token_id) {
            return true;
        }
        if let Some(time) = self.last_checked_metadata.get(token_id) {
            // Don't try to get metadata on every NEP-141 event if the metadata is corrupted
            if time.elapsed() < Duration::from_secs(60) && !force {
                return false;
            }
        }
        log::info!("Trying to add token {token_id}");
        match get_token_metadata(token_id.clone()).await {
            Ok(metadata) => {
                self.tokens.insert(
                    token_id.clone(),
                    Token {
                        account_id: token_id.clone(),
                        price_usd_raw: BigDecimal::from(0),
                        price_usd: BigDecimal::from(0),
                        price_usd_hardcoded: BigDecimal::from(0),
                        main_pool: None,
                        total_supply: get_total_supply(token_id).await.unwrap_or_default(),
                        circulating_supply: get_circulating_supply(token_id, false)
                            .await
                            .unwrap_or_default(),
                        circulating_supply_excluding_team: get_circulating_supply(token_id, true)
                            .await
                            .unwrap_or_default(),
                        reputation: Default::default(),
                        socials: Default::default(),
                        slug: Default::default(),
                        deleted: false,
                        reference: if let Some(reference) = metadata.reference.clone() {
                            get_reference(reference).await.unwrap_or_default()
                        } else {
                            Default::default()
                        },
                        metadata,
                        liquidity_usd: 0.0,
                        volume_usd_1h: 0.0,
                        volume_usd_24h: 0.0,
                        volume_usd_7d: 0.0,
                    },
                );
                true
            }
            Err(err) => {
                log::warn!("Couldn't get metadata for {token_id}: {err:?}");
                self.last_checked_metadata
                    .insert(token_id.clone(), Instant::now());
                false
            }
        }
    }

    pub async fn update_pool(&mut self, pool_id: &str, pool: PoolType, data: PoolData) {
        let tokens = [data.tokens.0.clone(), data.tokens.1.clone()];
        self.pools.insert(pool_id.to_string(), (pool, data));
        for token_id in tokens {
            let (main_pool, token_liquidity) = self.recalculate_token(&token_id);
            if let Some(pool) = main_pool {
                let token = if let Some(token) = self.tokens.get_mut(&token_id) {
                    token
                } else if self.add_token(&token_id, false).await {
                    self.tokens.get_mut(&token_id).unwrap()
                } else {
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
                token.liquidity_usd =
                    ToPrimitive::to_f64(&(token_liquidity * token.price_usd_raw.clone()))
                        .unwrap_or_default()
                        / 10f64.powi(USD_DECIMALS as i32)
                        * 2f64;
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
                        deleted: false,
                        reference: if let Some(reference) = metadata.reference.clone() {
                            get_reference(reference).await.unwrap_or_default()
                        } else {
                            Default::default()
                        },
                        metadata,
                        liquidity_usd: 0.0,
                        volume_usd_1h: 0.0,
                        volume_usd_24h: 0.0,
                        volume_usd_7d: 0.0,
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

    pub async fn search_tokens(
        &self,
        search: &str,
        take: usize,
        min_reputation: TokenScore,
        account_id: Option<AccountId>,
        parent: Option<AccountId>,
    ) -> Vec<&Token> {
        let owned_tokens = if let Some(account_id) = account_id {
            get_owned_tokens(account_id).await
        } else {
            HashMap::new()
        };
        self.tokens
            .values()
            .filter(|token| token.reputation >= min_reputation)
            .filter(|token| {
                if let Some(parent) = parent.as_ref() {
                    token.account_id.as_str().ends_with(&format!(".{parent}"))
                } else {
                    true
                }
            })
            .map(|token| {
                (
                    token,
                    token.sorting_score(search).saturating_mul(
                        match owned_tokens.get(&token.account_id) {
                            None => 10,
                            Some(0) => 12,
                            Some(1..) => 13,
                        },
                    ),
                )
            })
            .filter(|(_, score)| *score > 0)
            .sorted_by_key(|(_, score)| Reverse(*score))
            .map(|(token, _)| token)
            .take(take)
            .collect()
    }
}

#[cached(time = 3600)]
async fn get_owned_tokens(account_id: AccountId) -> HashMap<AccountId, u128> {
    #[derive(Debug, Deserialize)]
    struct Response {
        tokens: Vec<Token>,
        #[allow(dead_code)]
        account_id: AccountId,
    }

    #[derive(Debug, Deserialize)]
    struct Token {
        #[allow(dead_code)]
        last_update_block_height: Option<BlockHeight>,
        contract_id: AccountId,
        #[serde(with = "dec_format")]
        balance: Balance,
    }

    let url = format!("https://api.fastnear.com/v1/account/{account_id}/ft");
    match get_reqwest_client().get(&url).send().await {
        Ok(response) => match response.json::<Response>().await {
            Ok(response) => response
                .tokens
                .into_iter()
                .map(|ft| (ft.contract_id, ft.balance))
                .collect(),
            Err(e) => {
                log::warn!("Failed to parse response of FTs owned by {account_id}: {e:?}");
                HashMap::new()
            }
        },
        Err(e) => {
            log::warn!("Failed to get FTs owned by {account_id}: {e:?}");
            HashMap::new()
        }
    }
}

#[io_cached(time = 3600, disk = true, map_error = "|e| anyhow::anyhow!(e)")]
pub async fn get_reference(reference: String) -> Result<serde_json::Value, anyhow::Error> {
    if let Ok(url) = Url::parse(&reference) {
        if let Ok(response) = get_reqwest_client().get(url).send().await {
            let mut value = response.json::<serde_json::Value>().await?;
            strip_long_strings(&mut value);
            Ok(value)
        } else {
            anyhow::bail!("Failed to get reference from {reference}");
        }
    } else {
        let reference = reference.trim_start_matches("/ipfs/");
        if reference.is_empty() {
            return Ok(Default::default());
        }
        if let Ok(response) = get_reqwest_client()
            .get(format!("https://ipfs.io/ipfs/{reference}"))
            .send()
            .await
        {
            let mut value = response.json::<serde_json::Value>().await?;
            strip_long_strings(&mut value);
            Ok(value)
        } else {
            anyhow::bail!("Failed to get reference from {reference}");
        }
    }
}

const STRING_TOO_LONG_LENGTH: usize = 2000;

fn strip_long_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for value in object.values_mut() {
                strip_long_strings(value);
            }

            if serde_json::to_string(value).unwrap().len() > STRING_TOO_LONG_LENGTH {
                *value = serde_json::Value::String(
                    "<object too long to be included in prices.intear.tech>".to_string(),
                );
            }
        }
        serde_json::Value::Array(array) => {
            for value in array {
                strip_long_strings(value);
            }

            if serde_json::to_string(value).unwrap().len() > STRING_TOO_LONG_LENGTH {
                *value = serde_json::Value::String(
                    "<array too long to be included in prices.intear.tech>".to_string(),
                );
            }
        }
        serde_json::Value::String(string) => {
            if string.len() > STRING_TOO_LONG_LENGTH {
                *value = serde_json::Value::String(
                    "<string too long to be included in prices.intear.tech>".to_string(),
                );
            }
        }
        _ => {}
    }
}
