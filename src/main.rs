mod http_server;
mod network;
mod pool_data;
mod supply;
#[cfg(test)]
mod tests;
mod token;
mod token_metadata;
mod tokens;
mod utils;

use std::convert::Infallible;
use std::fmt::Debug;
use std::time::SystemTime;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    str::FromStr,
    sync::Arc,
};

use cached::proc_macro::cached;
use http_server::launch_http_server;
use inevents_redis::RedisEventStream;
use inindexer::near_indexer_primitives::types::{AccountId, BlockHeightDelta};
use intear_events::events::{
    newcontract::nep141::NewContractNep141Event, price::price_token::PriceTokenEvent,
    trade::trade_pool_change::TradePoolChangeEvent,
};
use itertools::Itertools;
use lazy_static::lazy_static;
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
use tokens::{get_reference, Tokens};
use tokio::sync::RwLock;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use tokio_util::sync::CancellationToken;

lazy_static! {
    static ref CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent("Intear Price Indexer")
        .build()
        .expect("Failed to create reqwest client");
}

pub fn get_reqwest_client() -> &'static reqwest::Client {
    &CLIENT
}

const SPAM_TOKENS_FILE: &str = "spam_tokens.txt";
const MAX_REDIS_EVENT_BUFFER_SIZE: usize = 1_000;

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

#[tokio::main]
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

    log::info!("Updating pools");
    let pools = tokens.read().await.pools.clone();
    for (pool_id, (pool, data)) in pools.iter() {
        tokens
            .write()
            .await
            .update_pool(pool_id, pool.clone(), data.clone(), 0)
            .await;
    }
    log::info!("Pools updated");

    let json_serialized = Arc::new(RwLock::new(None));
    let cancellation_token = CancellationToken::new();

    let mut join_handles = Vec::new();

    join_handles.push(tokio::spawn(launch_http_server(
        Arc::clone(&tokens),
        Arc::clone(&json_serialized),
    )));

    let tokens_clone = Arc::clone(&tokens);
    let cancellation_token_clone = cancellation_token.clone();
    join_handles.push(tokio::spawn(async move {
        RedisEventStream::<NewContractNep141Event>::new(
            create_redis_connection().await,
            NewContractNep141Event::ID,
        )
        .start_reading_events(
            "token_indexer_discovery",
            move |event| {
                let tokens = Arc::clone(&tokens_clone);
                async move {
                    tokio::spawn(async move {
                        let token_id = event.account_id;
                        if !tokens
                            .write()
                            .await
                            .add_token(&token_id, true, event.block_height)
                            .await
                        {
                            // Allow RPC to catch up
                            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                            if !tokens
                                .write()
                                .await
                                .add_token(&token_id, true, event.block_height)
                                .await
                            {
                                tokio::time::sleep(tokio::time::Duration::from_secs(7)).await;
                                if !tokens
                                    .write()
                                    .await
                                    .add_token(&token_id, true, event.block_height)
                                    .await
                                {
                                    log::warn!("Failed to add token {token_id} after 10 seconds");
                                }
                            }
                        }
                    });
                    Ok::<(), Infallible>(())
                }
            },
            || cancellation_token_clone.is_cancelled(),
        )
        .await
        .expect("Failed to read new token events");
    }));

    let cancellation_token_clone = cancellation_token.clone();
    let mut i = 0;
    let json_serialized_clone = Arc::clone(&json_serialized);
    join_handles.push(tokio::spawn(async move {
        RedisEventStream::<TradePoolChangeEvent>::new(
            create_redis_connection().await,
            TradePoolChangeEvent::ID,
        )
        .start_reading_event_vecs(
            "pair_price_extractor",
            move |events| {
                i += 1;
                let tokens = Arc::clone(&tokens);
                let json_serialized = Arc::clone(&json_serialized_clone);
                async move {
                    let mut token_price_stream = RedisEventStream::<PriceTokenEvent>::new(
                        create_redis_connection().await,
                        PriceTokenEvent::ID,
                    );
                    let Some(last_event) = events.last() else {
                        return Ok(());
                    };
                    for event in events.iter() {
                        if let Some(pool_data) = extract_pool_data(&event.pool) {
                            process_pool_change(event, &pool_data, &tokens, &mut token_price_stream)
                                .await;
                        } else {
                            log::warn!("Ratios can't be extracted from pool {}", event.pool_id);
                        }
                    }

                    token_price_stream
                        .flush_events(last_event.block_height, MAX_REDIS_EVENT_BUFFER_SIZE)
                        .await?;

                    let mut tokens_mut = tokens.read().await.clone();
                    tokens_mut.recalculate_prices();
                    for (token_id, token) in tokens_mut.tokens.iter_mut() {
                        if token.deleted {
                            continue;
                        }

                        let update_interval_blocks = match (token.volume_usd_24h, last_event.block_height - token.created_at) {
                            (_, ..1_000) => 1,
                            (_, ..10_000) => 10,
                            (_, ..100_000) => 30,
                            (..1_000.0, _) => 10000,
                            (..10_000.0, _) => 450,
                            (10_000.0.., _) => 60,
                            _ => BlockHeightDelta::MAX,
                        };
                        if i % update_interval_blocks != 0 {
                            continue;
                        }

                        let token_id = token_id.clone();
                        let reputation = token.reputation;

                        let (
                            total_supply_result,
                            circulating_supply_result,
                            circulating_supply_excluding_team_result,
                            volume_24h_result,
                        ) = tokio::join!(
                            get_total_supply(&token_id),
                            get_circulating_supply(&token_id, false),
                            get_circulating_supply(&token_id, true),
                            get_volume_24h(token_id.clone()),
                        );

                        match total_supply_result {
                            Ok(total_supply) => token.total_supply = total_supply,
                            Err(e) => {
                                if reputation >= TokenScore::NotFake {
                                    log::warn!("Failed to get total supply for {token_id}: {e:?}")
                                }
                            }
                        }

                        match circulating_supply_result {
                            Ok(circulating_supply) => token.circulating_supply = circulating_supply,
                            Err(e) => {
                                if reputation >= TokenScore::NotFake {
                                    log::warn!("Failed to get circulating supply for {token_id}: {e:?}")
                                }
                            }
                        }

                        match circulating_supply_excluding_team_result {
                            Ok(circulating_supply_excluding_team) => {
                                token.circulating_supply_excluding_team = circulating_supply_excluding_team
                            }
                            Err(e) => {
                                if reputation >= TokenScore::NotFake {
                                    log::warn!(
                                        "Failed to get circulating supply excluding team for {token_id}: {e:?}"
                                    )
                                }
                            }
                        }

                        match volume_24h_result {
                            Ok(volume_24h) => token.volume_usd_24h = volume_24h,
                            Err(e) => {
                                if reputation >= TokenScore::NotFake {
                                    log::warn!("Failed to get 24h volume for {token_id}: {e:?}")
                                }
                            }
                        }
                    }

                    let mut spam_pending = Vec::new();
                    let mut token_metadatas = HashMap::new();

                    for (token_id, token) in tokens_mut.tokens.iter_mut() {
                        if token.deleted {
                            continue;
                        }
                        if let Ok(token_metadata) = get_token_metadata(token_id.clone()).await {
                            if token.reference.is_null() {
                                if let Some(reference) = token_metadata.reference.as_ref() {
                                    token.reference = get_reference(reference.clone()).await.unwrap_or_default();
                                }
                            }
                            let meta_stringified = format!(
                                "Symbol: {}\nName: {}\n",
                                token_metadata.symbol.replace('\n', " "),
                                token_metadata.name.replace('\n', " ")
                            );
                            token_metadatas.insert(token_id.clone(), token_metadata);
                            if token.reputation == TokenScore::Unknown {
                                match is_spam(&meta_stringified).await {
                                    Ok(true) => {
                                        spam_pending.push(token_id.clone());
                                    },
                                    Ok(false) => {}
                                    Err(e) => log::warn!("Failed to check spam for {token_id}: {e:?}\nDetails:\n{meta_stringified}"),
                                }
                            }
                        }
                    }


                    if !spam_pending.is_empty() {
                        let mut file = OpenOptions::new()
                            .create(true)
                            .write(true)
                            .append(true)
                            .open(SPAM_TOKENS_FILE)
                            .await
                            .expect("Failed to open spam tokens file");
                        let existing_spam_list = tokens_mut.spam_tokens.clone();
                        for token in spam_pending.iter() {
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
                        for token in spam_pending.iter() {
                            if let Some(token) = tokens_mut.tokens.get_mut(token) {
                                token.reputation = TokenScore::Spam;
                            }
                        }
                        tokens_mut.spam_tokens.extend(spam_pending);
                    }

                    let mut prices_only = HashMap::new();
                    let mut ref_compatibility_format = HashMap::new();
                    let mut super_precise = HashMap::new();

                    let mut prices_only_with_hardcoded = HashMap::new();
                    let mut ref_compatibility_format_with_hardcoded = HashMap::new();
                    let mut super_precise_with_hardcoded = HashMap::new();

                    for (token_id, token) in tokens_mut.tokens.iter() {
                        if token.deleted {
                            continue;
                        }
                        if let Some(token_metadata) = token_metadatas.get(token_id) {
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

                            let update_interval_blocks = match (token.volume_usd_24h, last_event.block_height - token.created_at) {
                                (_, ..1_000) => 1,
                                (_, ..10_000) => 10,
                                (_, ..100_000) => 30,
                                (..1_000.0, _) => 10000,
                                (..10_000.0, _) => 450,
                                (..100_000.0, _) => 30,
                                (..1_000_000.0, _) => 15,
                                (1_000_000.0.., _) => 5,
                                _ => BlockHeightDelta::MAX,
                            };
                            if i % update_interval_blocks != 0 {
                                continue;
                            }

                            token_price_stream.add_event(PriceTokenEvent {
                                token: token_id.clone(),
                                price_usd: token.price_usd_raw.clone(),
                                timestamp_nanosec: SystemTime::now()
                                    .duration_since(SystemTime::UNIX_EPOCH)
                                    .unwrap()
                                    .as_nanos(),
                            });
                        }
                    }

                    token_price_stream
                        .flush_events(last_event.block_height, MAX_REDIS_EVENT_BUFFER_SIZE)
                        .await?;

                    let full_data = serde_json::to_string(&tokens_mut.tokens).unwrap();
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

                    *tokens.write().await = tokens_mut;
                    save_tokens(&*tokens.read().await).await;

                    Ok::<(), anyhow::Error>(())
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

async fn process_pool_change(
    event: &TradePoolChangeEvent,
    pool_data: &PoolData,
    tokens: &Arc<RwLock<Tokens>>,
    token_price_stream: &mut RedisEventStream<PriceTokenEvent>,
) {
    tokens
        .write()
        .await
        .update_pool(
            &event.pool_id,
            event.pool.clone(),
            pool_data.clone(),
            event.block_height,
        )
        .await;

    let token_read = tokens.read().await;
    for token_id in [&pool_data.tokens.0, &pool_data.tokens.1] {
        if let Some(token) = token_read.tokens.get(token_id) {
            let token_price_event = PriceTokenEvent {
                token: token_id.clone(),
                price_usd: token.price_usd_raw.clone(),
                timestamp_nanosec: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            };
            token_price_stream.add_event(token_price_event);
        }
    }
}

async fn create_redis_connection() -> ConnectionManager {
    ConnectionManager::new(
        Client::open(std::env::var("REDIS_URL").expect("REDIS_URL enviroment variable not set"))
            .expect("Failed to create Redis client"),
    )
    .await
    .expect("Failed to create Redis connection")
}

#[cached(time = 300, result = true)]
async fn get_volume_24h(token_id: AccountId) -> Result<f64, anyhow::Error> {
    Ok(get_reqwest_client()
        .get(format!(
            "https://events-v3.intear.tech/v3/trade_swap/volume_usd_24h?token_id={token_id}"
        ))
        .send()
        .await?
        .json::<f64>()
        .await?)
}
