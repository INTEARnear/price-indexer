use std::{collections::HashMap, env, str::FromStr, sync::Arc};

use actix_web::{http::StatusCode, web, App, HttpResponse, HttpResponseBuilder, HttpServer};
use cached::proc_macro::cached;
use inevents::events::event::Event;
use inevents_redis::RedisEventStream;
use inindexer::near_indexer_primitives::{
    types::{AccountId, BlockReference, Finality},
    views::QueryRequest,
};
use intear_events::events::{
    price::{
        price_pool::{PricePoolEvent, PricePoolEventData},
        price_token::{PriceTokenEvent, PriceTokenEventData},
    },
    trade::trade_pool_change::{PoolType, RefPool, TradePoolChangeEvent, TradePoolChangeEventData},
};
use itertools::Itertools;
use near_jsonrpc_client::{methods, JsonRpcClient};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use redis::{aio::ConnectionManager, Client};
use serde::Deserialize;
use sqlx::types::BigDecimal;
use tokio::sync::{Mutex, RwLock};

/// Used to ignore warnings for this token.
const KNOWN_TOKENS_WITH_NO_POOL: &[&str] = &[];

// Feel free to add other tokens in ROUTES if you're sure that the pools
// won't unexpectedly go to 0 without this change in the code. If the
// pool here is going to be 0, change it before removing liquidity.
const USD_TOKEN: &str = "usdt.tether-token.near";
const USD_ROUTES: &[(&str, &str)] = &[
    ("wrap.near", "REF-3879"), // NEAR-USDT
    // TODO USDC when stableswap is implemented
    // (
    //     "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1", // USDC
    //     "4179", // 4stables stableswap
    // ),
    // TODO FRAX when stableswap is implemented
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
        "REF-3804",            // NEKO-NEAR
    ),
];

const MAX_REDIS_EVENT_BUFFER_SIZE: usize = 10_000;

#[derive(Debug, Deserialize)]
struct JsonSerializedPrices {
    prices_only: String,
    ref_compatibility_format: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()
        .unwrap();

    let tokens = Arc::new(RwLock::new(Tokens::new()));

    let redis_connection = ConnectionManager::new(Client::open(
        std::env::var("REDIS_URL").expect("REDIS_URL enviroment variable not set"),
    )?)
    .await?;

    let tokens_clone = Arc::clone(&tokens);

    // This mutex is used to enforce strict order of event block heights. Even though the "every 5s for all tokens"
    // loop doesn't have a block height, it needs to exist in Redis and database because API clients use it.
    // While this mutex is locked, the block height can't be updated by pool change events.
    let last_event_block_height = Arc::new(Mutex::new(None));

    let mut token_price_stream = RedisEventStream::<PriceTokenEventData>::new(
        redis_connection.clone(),
        PriceTokenEvent::ID.to_string(),
    );

    let last_event_block_height_2 = Arc::new(Mutex::new(None));

    let json_serialized_all_tokens = Arc::new(RwLock::new(None));
    let json_serialized_all_tokens_2 = Arc::clone(&json_serialized_all_tokens);
    tokio::spawn(async move {
        let usd_metadata = get_token_metadata(USD_TOKEN.parse().unwrap())
            .await
            .expect("Failed to get USD metadata");
        loop {
            tokens_clone.write().await.recalculate_prices();
            let prices = tokens_clone
                .read()
                .await
                .tokens
                .iter()
                .map(|(token_id, token)| (token_id.clone(), token.clone()))
                .collect::<HashMap<_, _>>();
            let mut prices_only = HashMap::new();
            let mut ref_compatibility_format = HashMap::new();
            let last_block_height = last_event_block_height_2.lock().await;
            for (token_id, token) in prices {
                if let Ok(token_metadata) = get_token_metadata(token_id.clone()).await {
                    let price = &token.price_usd;
                    let price_usd = price
                        * BigDecimal::from_str(&(10u128.pow(token_metadata.decimals)).to_string())
                            .unwrap()
                        / BigDecimal::from_str(&(10u128.pow(usd_metadata.decimals)).to_string())
                            .unwrap();

                    let price_f64 = f64::from_str(&price_usd.to_string()).unwrap();
                    let price_ref_scale = price_usd.with_scale(12);

                    prices_only.insert(token_id.clone(), price_f64);
                    ref_compatibility_format.insert(
                        token_id.clone(),
                        serde_json::json!({
                            "price": price_ref_scale.to_string(),
                            "symbol": token.metadata.symbol,
                            "decimal": token_metadata.decimals,
                        }),
                    );

                    if let Some((block_height, block_timestamp_nanosec)) = *last_block_height {
                        token_price_stream
                            .emit_event(
                                0,
                                PriceTokenEventData {
                                    block_height,
                                    token: token_id,
                                    price_usd,
                                    timestamp_nanosec: block_timestamp_nanosec,
                                },
                                MAX_REDIS_EVENT_BUFFER_SIZE,
                            )
                            .await
                            .expect("Failed to emit price token event");
                    }
                }
            }
            drop(last_block_height);
            let json_serialized_prices_only = serde_json::to_string(&prices_only).unwrap();
            let json_serialized_ref_compatibility_format =
                serde_json::to_string(&ref_compatibility_format).unwrap();
            *json_serialized_all_tokens.write().await = Some(JsonSerializedPrices {
                prices_only: json_serialized_prices_only,
                ref_compatibility_format: json_serialized_ref_compatibility_format,
            });
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });

    tokio::spawn(async move {
        HttpServer::new(move || {
            let json_serialized_all_tokens_3 = Arc::clone(&json_serialized_all_tokens_2);
            let json_serialized_all_tokens_4 = Arc::clone(&json_serialized_all_tokens_2);
            App::new()
                .route(
                    "/list-token-price",
                    web::get().to(move || {
                        let json_serialized_all_tokens = Arc::clone(&json_serialized_all_tokens_3);
                        async move {
                            if let Some(json_serialized_all_tokens) =
                                json_serialized_all_tokens.read().await.as_ref()
                            {
                                HttpResponseBuilder::new(StatusCode::OK)
                                    .content_type("application/json")
                                    .body(
                                        json_serialized_all_tokens.ref_compatibility_format.clone(),
                                    )
                            } else {
                                HttpResponse::InternalServerError().finish()
                            }
                        }
                    }),
                )
                .route(
                    "/prices",
                    web::get().to(move || {
                        let json_serialized_all_tokens = Arc::clone(&json_serialized_all_tokens_4);
                        async move {
                            if let Some(json_serialized_all_tokens) =
                                json_serialized_all_tokens.read().await.as_ref()
                            {
                                HttpResponseBuilder::new(StatusCode::OK)
                                    .content_type("application/json")
                                    .body(json_serialized_all_tokens.prices_only.clone())
                            } else {
                                HttpResponse::InternalServerError().finish()
                            }
                        }
                    }),
                )
        })
        .disable_signals()
        .bind(env::var("BIND_ADDRESS").expect("BIND_ADDRESS environment variable not set"))
        .expect("Failed to bind HTTP server")
        .run()
        .await
        .expect("Failed to start HTTP server");
    });

    RedisEventStream::<TradePoolChangeEventData>::new(
        redis_connection.clone(),
        TradePoolChangeEvent::ID.to_string(),
    )
    .start_reading_events("pair_price_extractor", move |event| {
        let mut pool_price_stream = RedisEventStream::<PricePoolEventData>::new(
            redis_connection.clone(),
            PricePoolEvent::ID.to_string(),
        );
        let mut token_price_stream = RedisEventStream::<PriceTokenEventData>::new(
            redis_connection.clone(),
            PriceTokenEvent::ID.to_string(),
        );
        let tokens = Arc::clone(&tokens);
        let last_event_block_height = Arc::clone(&last_event_block_height);
        async move {
            last_event_block_height
                .lock()
                .await
                .replace((event.block_height, event.block_timestamp_nanosec));
            if let Some(pool_data) = extract_pool_data(&event.pool) {
                process_pool(&event, &pool_data, &mut pool_price_stream).await;
                process_token(&event, &pool_data, &mut token_price_stream, &tokens).await;
            } else {
                log::warn!("Ratios can't be extracted from pool {}", event.pool_id);
            }
            Ok(())
        }
    })
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Token {
    price_usd: BigDecimal,
    /// 'Main pool' is a pool that leads to a token that can be farther converted
    /// into [`USD_TOKEN`] through one of [`USD_ROUTES`].
    main_pool: Option<String>,
    metadata: TokenMetadata,
}

#[derive(Debug)]
struct Tokens {
    hardcoded_usd_routes: HashMap<AccountId, String>,

    tokens: HashMap<AccountId, Token>,
    pools: HashMap<String, (PoolType, PoolData)>,
}

impl Tokens {
    fn new() -> Self {
        let mut hardcoded_usd_routes = HashMap::new();
        for (token_id, pool_id) in USD_ROUTES {
            hardcoded_usd_routes.insert(token_id.parse().unwrap(), pool_id.to_string());
        }
        Self {
            hardcoded_usd_routes,
            tokens: HashMap::new(),
            pools: HashMap::new(),
        }
    }

    fn recalculate_token(&self, token_id: &AccountId) -> Option<String> {
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

    async fn update_pool(&mut self, pool_id: &str, pool: PoolType, data: PoolData) {
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

    fn get_price(&self, token_id: &AccountId) -> Option<BigDecimal> {
        let token = self.tokens.get(token_id)?;
        Some(if token_id == USD_TOKEN {
            1.into()
        } else {
            token.price_usd.clone()
        })
    }

    fn recalculate_prices(&mut self) {
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

fn calculate_price(
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

#[derive(Debug, Clone)]
struct PoolData {
    tokens: (AccountId, AccountId),
    ratios: (BigDecimal, BigDecimal),
    liquidity: (BigDecimal, BigDecimal),
}

fn extract_pool_data(pool: &PoolType) -> Option<PoolData> {
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

async fn process_pool(
    event: &TradePoolChangeEventData,
    pool_data: &PoolData,
    price_stream: &mut RedisEventStream<PricePoolEventData>,
) {
    let price_event = PricePoolEventData {
        block_height: event.block_height,
        pool_id: event.pool_id.clone(),
        token0: pool_data.tokens.0.clone(),
        token1: pool_data.tokens.1.clone(),
        token0_in_1_token1: pool_data.ratios.0.clone(),
        token1_in_1_token0: pool_data.ratios.1.clone(),
        timestamp_nanosec: event.block_timestamp_nanosec,
    };
    price_stream
        .emit_event(event.block_height, price_event, MAX_REDIS_EVENT_BUFFER_SIZE)
        .await
        .expect("Failed to emit price pool event");
}

async fn process_token(
    event: &TradePoolChangeEventData,
    pool_data: &PoolData,
    price_stream: &mut RedisEventStream<PriceTokenEventData>,
    tokens: &Arc<RwLock<Tokens>>,
) {
    let mut tokens_write = tokens.write().await;
    tokens_write
        .update_pool(&event.pool_id, event.pool.clone(), pool_data.clone())
        .await;
    drop(tokens_write);

    let token_read = tokens.read().await;
    let mut events = Vec::new();
    for token_id in [&pool_data.tokens.0, &pool_data.tokens.1] {
        let price_event = PriceTokenEventData {
            block_height: event.block_height,
            token: token_id.clone(),
            price_usd: token_read.get_price(token_id).unwrap(),
            timestamp_nanosec: event.block_timestamp_nanosec,
        };
        events.push(price_event);
    }
    drop(token_read);

    for event in events {
        price_stream
            .emit_event(event.block_height, event, MAX_REDIS_EVENT_BUFFER_SIZE)
            .await
            .expect("Failed to emit price token event");
    }
}

#[tokio::test]
async fn test_prices() {
    // TODO split this into multiple tests
    const NEAR_DECIMALS: u32 = 24;
    const USD_DECIMALS: u32 = 6;
    const INTEL_DECIMALS: u32 = 18;
    const CHADS_DECIMALS: u32 = 18;

    use intear_events::events::trade::trade_pool_change::*;

    let mut tokens = Tokens::new();
    assert!(tokens.get_price(&"wrap.near".parse().unwrap()).is_none());

    // USDT pool
    let near_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "wrap.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![85_000e24 as u128, 450_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let near_usdt_data = extract_pool_data(&near_usdt_pool).unwrap();

    tokens
        .update_pool("REF-3879", near_usdt_pool, near_usdt_data)
        .await;
    assert_eq!(
        (tokens.get_price(&"wrap.near".parse().unwrap()).unwrap()
            * BigDecimal::from_str(&(10u128.pow(NEAR_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "5.29"
    );

    eprintln!("---");

    // NEAR pool
    let intel_near_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "wrap.near".parse().unwrap(),
        ],
        amounts: vec![21_000_000_000e18 as u128, 3_000e24 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_near_pool).unwrap();

    tokens
        .update_pool("REF-4663", intel_near_pool, intel_near_data)
        .await;
    assert_eq!(
        (tokens
            .get_price(&"intel.tkn.near".parse().unwrap())
            .unwrap()
            * BigDecimal::from_str(&(10u128.pow(INTEL_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "0.000000756"
    );

    eprintln!("---");

    // Other token (intel) pool
    let chads_intel_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "chads.tkn.near".parse().unwrap(),
        ],
        amounts: vec![1_666_666_666e18 as u128, 1_666e18 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let chads_intel_data = extract_pool_data(&chads_intel_pool).unwrap();

    tokens
        .update_pool("REF-4774", chads_intel_pool, chads_intel_data)
        .await;
    assert_eq!(
        (tokens
            .get_price(&"chads.tkn.near".parse().unwrap())
            .unwrap()
            * BigDecimal::from_str(&(10u128.pow(CHADS_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "0.757"
    );

    eprintln!("---");

    // Update existing pool, lower token liquidity
    let chads_intel_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "chads.tkn.near".parse().unwrap(),
        ],
        amounts: vec![2_000_000_000e18 as u128, 1_500e18 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let chads_intel_data = extract_pool_data(&chads_intel_pool).unwrap();

    tokens
        .update_pool("REF-4774", chads_intel_pool, chads_intel_data)
        .await;
    assert_eq!(
        (tokens
            .get_price(&"chads.tkn.near".parse().unwrap())
            .unwrap()
            * BigDecimal::from_str(&(10u128.pow(CHADS_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "1.01"
    );

    eprintln!("---");

    // Add new pool with lower token liquidity, price shouldn't change
    let intel_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![20_000_000_000e18 as u128, 20_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_usdt_pool).unwrap();

    tokens
        .update_pool("TEST-69", intel_usdt_pool, intel_near_data)
        .await;
    assert_eq!(
        (tokens
            .get_price(&"intel.tkn.near".parse().unwrap())
            .unwrap()
            * BigDecimal::from_str(&(10u128.pow(INTEL_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "0.000000756"
    );

    eprintln!("---");

    // Add new pool with higher token liquidity, price should change
    let intel_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![200_000_000_000e18 as u128, 200_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_usdt_pool).unwrap();

    tokens
        .update_pool("TEST-42", intel_usdt_pool, intel_near_data)
        .await;
    assert_eq!(
        (tokens
            .get_price(&"intel.tkn.near".parse().unwrap())
            .unwrap()
            * BigDecimal::from_str(&(10u128.pow(INTEL_DECIMALS)).to_string()).unwrap()
            / BigDecimal::from_str(&(10u128.pow(USD_DECIMALS)).to_string()).unwrap())
        .with_prec(3)
        .to_string(),
        "0.00000100"
    );
}

const RPC_URL: &str = "https://rpc.shitzuapes.xyz";

#[cached(time = 3600, result = true)]
async fn get_token_metadata(token_id: AccountId) -> anyhow::Result<TokenMetadata> {
    let client = JsonRpcClient::connect(RPC_URL);
    let request = methods::query::RpcQueryRequest {
        block_reference: BlockReference::Finality(Finality::Final),
        request: QueryRequest::CallFunction {
            account_id: token_id,
            method_name: "ft_metadata".into(),
            args: Vec::new().into(),
        },
    };
    let response = client.call(request).await?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    Ok(serde_json::from_slice(&call_result.result)?)
}

#[derive(Debug, Deserialize, Clone)]
struct TokenMetadata {
    decimals: u32,
    symbol: String,
}

#[tokio::test]
async fn test_get_decimals() {
    assert_eq!(
        get_token_metadata("wrap.near".parse().unwrap())
            .await
            .unwrap()
            .decimals,
        24,
    );
    assert_eq!(
        get_token_metadata("usdt.tether-token.near".parse().unwrap())
            .await
            .unwrap()
            .symbol,
        "USDt",
    );
    assert_eq!(
        get_token_metadata("intel.tkn.near".parse().unwrap())
            .await
            .unwrap()
            .decimals,
        18,
    );
}
