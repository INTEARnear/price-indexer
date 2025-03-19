use anyhow::Result;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::get_reqwest_client;
use crate::utils::{
    JsonRpcSupportedTokensRequest, JsonRpcSupportedTokensResponse, SupportedTokensRequest,
};

lazy_static! {
    static ref SOLANA_PRICES: Arc<RwLock<HashMap<String, f64>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

pub fn get_jupiter_price(address: &str) -> Option<f64> {
    SOLANA_PRICES.read().get(address).copied()
}

/// Generates `sol-<THIS>.omft.near` part
pub fn derive_near_address_from_solana(address: &str) -> Result<String, bs58::decode::Error> {
    let mut hasher = Keccak256::new();
    let address_bytes = bs58::decode(address).into_vec()?;
    hasher.update(&address_bytes);
    let hash = hasher.finalize();
    let shortened = &hash[12..];
    let hex_string = hex::encode(shortened);
    Ok(hex_string)
}

#[derive(Debug, Serialize, Deserialize)]
struct JupiterPriceResponse {
    data: HashMap<String, Option<JupiterTokenPrice>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JupiterTokenPrice {
    id: String,
    #[serde(rename = "type")]
    price_type: String,
    price: String,
}

async fn get_jupiter_prices(tokens: &[String]) -> Result<HashMap<String, f64>> {
    let client = get_reqwest_client();
    let mut all_prices = HashMap::new();

    // Process tokens in chunks of 5
    for chunk in tokens.chunks(5) {
        let ids = chunk.join(",");
        let url = format!("https://api.jup.ag/price/v2?ids={ids}");

        let response: JupiterPriceResponse = client.get(&url).send().await?.json().await?;

        all_prices.extend(response.data.into_iter().filter_map(|(id, price_data)| {
            price_data
                .and_then(|data| data.price.parse::<f64>().ok())
                .map(|p| (id, p))
        }));
    }

    Ok(all_prices)
}

async fn update_solana_tokens() -> Result<()> {
    let client = get_reqwest_client();
    let request = JsonRpcSupportedTokensRequest {
        id: 1,
        jsonrpc: "2.0".to_string(),
        method: "supported_tokens".to_string(),
        params: vec![SupportedTokensRequest {
            chains: vec!["sol:mainnet".to_string()],
        }],
    };

    let response: JsonRpcSupportedTokensResponse = client
        .post("https://bridge.chaindefuser.com/rpc")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await?
        .json()
        .await?;

    let tokens = response.result.tokens;

    let token_addresses: Vec<String> = tokens
        .iter()
        .filter(|t| t.defuse_asset_identifier != "sol:mainnet:native")
        .filter_map(|t| {
            t.defuse_asset_identifier
                .strip_prefix("sol:mainnet:")
                .map(String::from)
        })
        .collect();

    let prices = get_jupiter_prices(&token_addresses).await?;

    let mut price_map = SOLANA_PRICES.write();
    price_map.clear();

    for ref identifier in token_addresses {
        if let Ok(transformed) = derive_near_address_from_solana(identifier) {
            if let Some(price) = prices.get(identifier) {
                price_map.insert(transformed.to_string(), *price);
            }
        }
    }

    Ok(())
}

pub async fn subscribe_to_solana_updates() -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        if let Err(e) = update_solana_tokens().await {
            log::error!("Failed to update Solana tokens: {e}");
        }
    }
}
