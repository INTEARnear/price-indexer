mod http_server;
mod pool_data;
mod supply;
#[cfg(test)]
mod tests;
mod token;
mod token_metadata;
mod tokens;
mod utils;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use http_server::launch_http_server;
use inevents_redis::RedisEventStream;
use intear_events::events::{
    newcontract::nep141::{NewContractNep141Event, NewContractNep141EventData},
    price::{
        price_pool::{PricePoolEvent, PricePoolEventData},
        price_token::{PriceTokenEvent, PriceTokenEventData},
    },
    trade::trade_pool_change::{TradePoolChangeEvent, TradePoolChangeEventData},
};
use itertools::Itertools;
use near_jsonrpc_client::{
    errors::{JsonRpcError, JsonRpcServerError},
    methods::query::RpcQueryError,
};
use pool_data::{extract_pool_data, PoolData};
use redis::{aio::ConnectionManager, Client};
use serde::Deserialize;
use supply::{get_circulating_supply, get_total_supply};
use token::{get_reputation, get_slug, get_socials, is_spam, TokenScore};
use token_metadata::{get_token_metadata, MetadataError};
use tokens::Tokens;
use tokio::sync::{Mutex, RwLock};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use tokio_util::sync::CancellationToken;

const SPAM_TOKENS_FILE: &str = "spam_tokens.txt";
const MAX_REDIS_EVENT_BUFFER_SIZE: usize = 100_000;

#[derive(Debug, Deserialize)]
struct JsonSerializedPrices {
    prices_only: String,
    ref_compatibility_format: String,
    super_precise: String,

    prices_only_with_hardcoded: String,
    ref_compatibility_format_with_hardcoded: String,
    super_precise_with_hardcoded: String,

    full_data: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()
        .unwrap();

    let mut tokens = load_tokens().await.expect("Failed to load tokens");
    log::info!("Updating metadata of {} tokens", tokens.tokens.len());
    for (account_id, token) in tokens.tokens.iter_mut() {
        if token.deleted {
            continue;
        }
        token.account_id = account_id.clone();
        token.reputation = get_reputation(account_id, &tokens.spam_tokens);
        token.slug = get_slug(account_id)
            .into_iter()
            .map(|s| s.to_owned())
            .collect();
        token.socials = get_socials(account_id)
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        match get_token_metadata(account_id.clone()).await {
            Ok(metadata) => token.metadata = metadata,
            Err(MetadataError::RpcQueryError(JsonRpcError::ServerError(
                JsonRpcServerError::HandlerError(RpcQueryError::UnknownAccount { .. }),
            ))) => {
                log::warn!("Token {account_id} doesn't exist, marking as deleted");
                token.deleted = true;
            }
            Err(e) => {
                if token.reputation >= TokenScore::NotFake {
                    log::warn!("Failed to get metadata for {account_id}: {e:?}")
                }
            }
        }
    }
    log::info!("Metadata updated");
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

    let block_lock = Arc::new(Mutex::new(None));

    let json_serialized = Arc::new(RwLock::new(None));

    let cancellation_token = CancellationToken::new();
    let mut join_handles = Vec::new();

    join_handles.push(tokio::spawn(launch_http_server(
        Arc::clone(&tokens),
        Arc::clone(&json_serialized),
    )));

    let token_price_stream = Arc::new(RedisEventStream::<PriceTokenEventData>::new(
        redis_connection.clone(),
        PriceTokenEvent::ID,
    ));
    let pool_price_stream = Arc::new(RedisEventStream::<PricePoolEventData>::new(
        redis_connection.clone(),
        PricePoolEvent::ID,
    ));

    let cancellation_token_clone = cancellation_token.clone();
    let token_price_stream_clone = Arc::clone(&token_price_stream);
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    join_handles.push(tokio::spawn(async move {
        loop {
            if cancellation_token_clone.is_cancelled() {
                break;
            }
            let mut tokens = tokens_clone.write().await;

            tokens.recalculate_prices();

            let mut prices_only = HashMap::new();
            let mut ref_compatibility_format = HashMap::new();
            let mut super_precise = HashMap::new();

            let mut prices_only_with_hardcoded = HashMap::new();
            let mut ref_compatibility_format_with_hardcoded = HashMap::new();
            let mut super_precise_with_hardcoded = HashMap::new();

            for (token_id, token) in tokens.tokens.iter_mut() {
                if token.deleted {
                    continue;
                }
                match get_total_supply(token_id).await {
                    Ok(total_supply) => token.total_supply = total_supply,
                    Err(e) => {
                        if token.reputation >= TokenScore::NotFake {
                            log::warn!("Failed to get total supply for {token_id}: {e:?}")
                        }
                    }
                }
                match get_circulating_supply(token_id, false).await {
                    Ok(circulating_supply) => token.circulating_supply = circulating_supply,
                    Err(e) => {
                        if token.reputation >= TokenScore::NotFake {
                            log::warn!("Failed to get circulating supply for {token_id}: {e:?}")
                        }
                    }
                }
                match get_circulating_supply(token_id, true).await {
                    Ok(circulating_supply_excluding_team) => {
                        token.circulating_supply_excluding_team = circulating_supply_excluding_team
                    }
                    Err(e) => {
                        if token.reputation >= TokenScore::NotFake {
                            log::warn!(
                                "Failed to get circulating supply excluding team for {token_id}: {e:?}"
                            )
                        }
                    }
                }
            }

            drop(tokens);
            let tokens = tokens_clone.read().await;
            let mut to_add_in_spam = Vec::new();
            let block_lock_clone = block_lock.lock().await;

            for (token_id, token) in tokens.tokens.iter() {
                if token.deleted {
                    continue;
                }
                if let Ok(token_metadata) = get_token_metadata(token_id.clone()).await {
                    let meta_stringified = format!(
                        "Symbol: {}\nName: {}\n",
                        token_metadata.symbol.replace('\n', " "),
                        token_metadata.name.replace('\n', " ")
                    );
                    if token.reputation == TokenScore::Unknown {
                        match is_spam(&meta_stringified).await {
                            Ok(true) => {
                                to_add_in_spam.push(token_id.clone());
                            },
                            Ok(false) => {}
                            Err(e) => log::warn!("Failed to check spam for {token_id}: {e:?}\nDetails:\n{meta_stringified}"),
                        }
                    }

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

                    if let Some((block_height, block_timestamp_nanosec)) = *block_lock_clone {
                        if let Err(err) = token_price_stream_clone
                            .emit_event(
                                0,
                                PriceTokenEventData {
                                    block_height,
                                    token: token_id.clone(),
                                    price_usd: token.price_usd_raw.clone(),
                                    timestamp_nanosec: block_timestamp_nanosec,
                                },
                                MAX_REDIS_EVENT_BUFFER_SIZE,
                            ) {
                                log::error!("Failed to emit price token event: {err:?}");
                            }
                    }
                }
            }
            drop(block_lock_clone);
            let full_data = serde_json::to_string(&tokens.tokens).unwrap();
            drop(tokens);
            let ref_compatibility_format = ref_compatibility_format
                .into_iter()
                .sorted_by_key(|(token_id, _)| token_id.to_string())
                .collect::<BTreeMap<_, _>>();
            *json_serialized.write().await = Some(JsonSerializedPrices {
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

                full_data,
            });
            log::info!("Updated all prices");

            if !to_add_in_spam.is_empty() {
                let mut file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(true)
                    .open(SPAM_TOKENS_FILE)
                    .await
                    .expect("Failed to open spam tokens file");
                let existing_spam_list = tokens_clone.read().await.spam_tokens.clone();
                for token in to_add_in_spam.iter() {
                    if !existing_spam_list.contains(token) {
                        file.write_all(format!("{token} // added by AI\n").as_bytes())
                            .await
                            .expect("Failed to write spam token");
                    }
                }
                file.flush()
                    .await
                    .expect("Failed to flush spam tokens file");
                drop(file);
                let mut tokens = tokens_clone.write().await;
                for token in to_add_in_spam.iter() {
                    if let Some(token) = tokens.tokens.get_mut(token) {
                        token.reputation = TokenScore::Spam;
                    }
                }
                tokens.spam_tokens.extend(to_add_in_spam);
            }

            save_tokens(&*tokens_clone.read().await).await;
            interval.tick().await;
        }
    }));

    let tokens_clone = Arc::clone(&tokens);
    let redis_connection_clone = redis_connection.clone();
    let cancellation_token_clone = cancellation_token.clone();
    join_handles.push(tokio::spawn(async move {
        RedisEventStream::<NewContractNep141EventData>::new(
            redis_connection_clone,
            NewContractNep141Event::ID,
        )
        .start_reading_events(
            "token_indexer_discovery",
            move |event| {
                let tokens = Arc::clone(&tokens_clone);
                async move {
                    let token_id = event.account_id;
                    tokens.write().await.add_token(&token_id).await;
                    Ok(())
                }
            },
            || cancellation_token_clone.is_cancelled(),
        )
        .await
        .expect("Failed to read new token events");
    }));

    let cancellation_token_clone = cancellation_token.clone();
    let pool_price_stream_clone = Arc::clone(&pool_price_stream);
    let token_price_stream_clone = Arc::clone(&token_price_stream);
    join_handles.push(tokio::spawn(async move {
        RedisEventStream::<TradePoolChangeEventData>::new(
            redis_connection.clone(),
            TradePoolChangeEvent::ID,
        )
        .start_reading_events(
            "pair_price_extractor",
            move |event| {
                let tokens = Arc::clone(&tokens);
                let last_event_block_height = Arc::clone(&last_event_block_height);
                let pool_price_stream = Arc::clone(&pool_price_stream_clone);
                let token_price_stream = Arc::clone(&token_price_stream_clone);
                async move {
                    last_event_block_height
                        .lock()
                        .await
                        .replace((event.block_height, event.block_timestamp_nanosec));
                    if let Some(pool_data) = extract_pool_data(&event.pool) {
                        process_pool(&event, &pool_data, &pool_price_stream).await;
                        process_token(&event, &pool_data, &token_price_stream, &tokens).await;
                    } else {
                        log::warn!("Ratios can't be extracted from pool {}", event.pool_id);
                    }
                    Ok(())
                }
            },
            || cancellation_token_clone.is_cancelled(),
        )
        .await
        .expect("Failed to read trade pool change events");
    }));

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to wait for Ctrl+C");
    log::info!("Ctrl+C received, waiting for tasks to finish");
    cancellation_token.cancel();
    for (i, join_handle) in join_handles.into_iter().enumerate() {
        log::info!("Stopping task {}", i + 1);
        join_handle.await?;
    }
    log::info!("Sending all remaining token events");
    Arc::try_unwrap(token_price_stream)
        .map_err(|_| anyhow::anyhow!("Failed to unwrap token price stream"))?
        .stop()
        .await;
    log::info!("Sending all remaining pool events");
    Arc::try_unwrap(pool_price_stream)
        .map_err(|_| anyhow::anyhow!("Failed to unwrap pool price stream"))?
        .stop()
        .await;
    log::info!("Exiting");

    Ok(())
}

async fn load_tokens() -> Result<Tokens, anyhow::Error> {
    if fs::try_exists("tokens.json").await? {
        let tokens = fs::read_to_string("tokens.json").await?;
        Ok(serde_json::from_str(&tokens)?)
    } else {
        Ok(Tokens::new())
    }
    .map(|mut tokens| {
        let mut spam_tokens = HashSet::new();
        if let Ok(file) = File::open(SPAM_TOKENS_FILE) {
            let mut buf = BufReader::new(file);
            let mut line = String::new();
            while let Ok(1..) = buf.read_line(&mut line) {
                let token_id = line.split("//").next().unwrap().trim().parse().unwrap();
                line.clear();
                spam_tokens.insert(token_id);
            }
            tokens.spam_tokens = spam_tokens;
        } else {
            log::warn!("Failed to open spam tokens file. Doesn it not exist?");
        }
        tokens
    })
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
    pool_price_stream: &RedisEventStream<PricePoolEventData>,
) {
    let pool_price_event = PricePoolEventData {
        block_height: event.block_height,
        pool_id: event.pool_id.clone(),
        token0: pool_data.tokens.0.clone(),
        token1: pool_data.tokens.1.clone(),
        token0_in_1_token1: pool_data.ratios.0.clone(),
        token1_in_1_token0: pool_data.ratios.1.clone(),
        timestamp_nanosec: event.block_timestamp_nanosec,
    };
    if let Err(err) = pool_price_stream.emit_event(
        event.block_height,
        pool_price_event,
        MAX_REDIS_EVENT_BUFFER_SIZE,
    ) {
        log::error!("Failed to emit price pool event: {err:?}");
    }
}

async fn process_token(
    event: &TradePoolChangeEventData,
    pool_data: &PoolData,
    token_price_stream: &RedisEventStream<PriceTokenEventData>,
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
            let token_price_event = PriceTokenEventData {
                block_height: event.block_height,
                token: token_id.clone(),
                price_usd: token.price_usd_raw.clone(),
                timestamp_nanosec: event.block_timestamp_nanosec,
            };
            events.push(token_price_event);
        }
    }
    drop(token_read);

    for event in events {
        if let Err(err) =
            token_price_stream.emit_event(event.block_height, event, MAX_REDIS_EVENT_BUFFER_SIZE)
        {
            log::error!("Failed to emit price token event: {err:?}");
        }
    }
}
