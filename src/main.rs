mod http_server;
mod pool_data;
#[cfg(test)]
mod tests;
mod token;
mod token_metadata;
mod tokens;
mod utils;

use std::{
    collections::{BTreeMap, HashMap},
    str::FromStr,
    sync::Arc,
};

use http_server::launch_http_server;
use inevents_redis::RedisEventStream;
use inindexer::near_indexer_primitives::types::AccountId;
use intear_events::events::{
    price::{
        price_pool::{PricePoolEvent, PricePoolEventData},
        price_token::{PriceTokenEvent, PriceTokenEventData},
    },
    trade::trade_pool_change::{TradePoolChangeEvent, TradePoolChangeEventData},
};
use itertools::Itertools;
use pool_data::{extract_pool_data, PoolData};
use redis::{aio::ConnectionManager, Client};
use serde::Deserialize;
use token_metadata::get_token_metadata;
use tokens::Tokens;
use tokio::fs;
use tokio::sync::{Mutex, RwLock};

const MAX_REDIS_EVENT_BUFFER_SIZE: usize = 10_000;

#[derive(Debug, Deserialize)]
struct JsonSerializedPrices {
    prices_only: String,
    ref_compatibility_format: String,
    super_precise: String,

    prices_only_with_hardcoded: String,
    ref_compatibility_format_with_hardcoded: String,
    super_precise_with_hardcoded: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()
        .unwrap();

    let tokens = if let Some(tokens) = load_tokens_save().await {
        tokens
    } else {
        Tokens::new()
    };
    let tokens = Arc::new(RwLock::new(tokens));

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

    tokio::spawn(launch_http_server(
        Arc::clone(&tokens),
        Arc::clone(&json_serialized_all_tokens),
    ));

    tokio::spawn(async move {
        loop {
            tokens_clone.write().await.recalculate_prices();
            let tokens = tokens_clone.read().await;

            let last_block_height = last_event_block_height_2.lock().await;

            let mut prices_only = HashMap::new();
            let mut ref_compatibility_format = HashMap::new();
            let mut super_precise = HashMap::new();

            let mut prices_only_with_hardcoded = HashMap::new();
            let mut ref_compatibility_format_with_hardcoded = HashMap::new();
            let mut super_precise_with_hardcoded = HashMap::new();

            for (token_id, token) in tokens.tokens.iter() {
                if let Ok(token_metadata) = get_token_metadata(token_id.clone()).await {
                    let price_usd = &token.price_usd;

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
                    super_precise.insert(token_id.clone(), price_usd.to_string());

                    let price_usd_hardcoded = &token.price_usd_hardcoded;
                    let price_f64_hardcoded =
                        f64::from_str(&price_usd_hardcoded.to_string()).unwrap();
                    let price_ref_scale_hardcoded = price_usd_hardcoded.with_scale(12);
                    prices_only_with_hardcoded.insert(token_id.clone(), price_f64_hardcoded);
                    ref_compatibility_format_with_hardcoded.insert(
                        token_id.clone(),
                        serde_json::json!({
                            "price": price_ref_scale_hardcoded.to_string(),
                            "symbol": token.metadata.symbol,
                            "decimal": token_metadata.decimals,
                        }),
                    );
                    super_precise_with_hardcoded
                        .insert(token_id.clone(), price_usd_hardcoded.to_string());

                    if let Some((block_height, block_timestamp_nanosec)) = *last_block_height {
                        token_price_stream
                            .emit_event(
                                0,
                                PriceTokenEventData {
                                    block_height,
                                    token: token_id.clone(),
                                    price_usd: token.price_usd_raw.clone(),
                                    timestamp_nanosec: block_timestamp_nanosec,
                                },
                                MAX_REDIS_EVENT_BUFFER_SIZE,
                            )
                            .expect("Failed to emit price token event");
                    }
                }
            }
            drop(last_block_height);
            drop(tokens);
            let ref_compatibility_format = ref_compatibility_format
                .into_iter()
                .sorted_by_key(|(token_id, _)| token_id.to_string())
                .collect::<BTreeMap<_, _>>();
            *json_serialized_all_tokens.write().await = Some(JsonSerializedPrices {
                prices_only: serde_json::to_string(&prices_only).unwrap(),
                ref_compatibility_format: serde_json::to_string(&ref_compatibility_format).unwrap(),
                super_precise: serde_json::to_string(&super_precise).unwrap(),

                prices_only_with_hardcoded: serde_json::to_string(&prices_only_with_hardcoded)
                    .unwrap(),
                ref_compatibility_format_with_hardcoded: serde_json::to_string(
                    &ref_compatibility_format_with_hardcoded,
                )
                .unwrap(),
                super_precise_with_hardcoded: serde_json::to_string(&super_precise_with_hardcoded)
                    .unwrap(),
            });
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            save_tokens(&*tokens_clone.read().await).await;
        }
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

#[derive(Debug, Deserialize)]
struct TokenIdWrapper {
    token_id: AccountId,
}

async fn load_tokens_save() -> Option<Tokens> {
    let tokens = fs::read_to_string("tokens.json").await.ok()?;
    serde_json::from_str(&tokens).ok()
}

async fn save_tokens(tokens: &Tokens) {
    let tokens = serde_json::to_string(tokens).unwrap();
    fs::write("tokens.json", tokens)
        .await
        .expect("Failed to save tokens");
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
        if let Some(token) = token_read.tokens.get(token_id) {
            let price_event = PriceTokenEventData {
                block_height: event.block_height,
                token: token_id.clone(),
                price_usd: token.price_usd_raw.clone(),
                timestamp_nanosec: event.block_timestamp_nanosec,
            };
            events.push(price_event);
        }
    }
    drop(token_read);

    for event in events {
        price_stream
            .emit_event(event.block_height, event, MAX_REDIS_EVENT_BUFFER_SIZE)
            .expect("Failed to emit price token event");
    }
}
