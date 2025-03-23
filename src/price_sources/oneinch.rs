use crate::get_reqwest_client;
use crate::utils::{
    JsonRpcSupportedTokensRequest, JsonRpcSupportedTokensResponse, SupportedTokensRequest,
};
use anyhow::Result;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const GECKOTERMINAL_BASE_URL: &str = "https://api.geckoterminal.com/api/v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Ethereum,
    Polygon,
    Bsc,
    Arbitrum,
    Base,
    Gnosis,
}

impl Network {
    fn chain_id_for_intents(&self) -> &'static str {
        match self {
            Network::Ethereum => "eth:1",
            Network::Polygon => "eth:137",
            Network::Bsc => "eth:56",
            Network::Arbitrum => "eth:42161",
            Network::Base => "eth:8453",
            Network::Gnosis => "eth:100",
        }
    }

    fn network_id_for_geckoterminal(&self) -> &'static str {
        match self {
            Network::Ethereum => "eth",
            Network::Polygon => "polygon_pos",
            Network::Bsc => "bsc",
            Network::Arbitrum => "arbitrum",
            Network::Base => "base",
            Network::Gnosis => "xdai",
        }
    }
}

lazy_static! {
    static ref ONEINCH_PRICES: Arc<RwLock<HashMap<(Network, String), f64>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

#[derive(Debug, Deserialize)]
struct GeckoTerminalResponse {
    data: TokenPrice,
}

#[derive(Debug, Deserialize)]
struct TokenPrice {
    attributes: TokenPriceAttributes,
}

#[derive(Debug, Deserialize)]
struct TokenPriceAttributes {
    token_prices: HashMap<String, String>,
}

pub fn get_oneinch_price(network: Network, address: &str) -> Option<f64> {
    ONEINCH_PRICES
        .read()
        .get(&(network, address.to_string()))
        .copied()
}

async fn update_oneinch_tokens(network: Network) -> Result<()> {
    let client = get_reqwest_client();

    let request = JsonRpcSupportedTokensRequest {
        id: 1,
        jsonrpc: "2.0".to_string(),
        method: "supported_tokens".to_string(),
        params: vec![SupportedTokensRequest {
            chains: vec![network.chain_id_for_intents().to_string()],
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
        .filter(|t| {
            t.defuse_asset_identifier != format!("{}:native", network.chain_id_for_intents())
        })
        .filter_map(|t| {
            t.defuse_asset_identifier
                .strip_prefix(&format!("{}:", network.chain_id_for_intents()))
                .map(String::from)
        })
        .collect();

    let url = format!(
        "{}/simple/networks/{}/token_price/{}",
        GECKOTERMINAL_BASE_URL,
        network.network_id_for_geckoterminal(),
        token_addresses.join(",")
    );

    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await?
        .json::<GeckoTerminalResponse>()
        .await?;

    let mut prices = ONEINCH_PRICES.write();

    for (address, price_str) in response.data.attributes.token_prices {
        if let Ok(price) = price_str.parse::<f64>() {
            prices.insert((network, address.to_lowercase()), price);
        }
    }

    Ok(())
}

pub async fn subscribe_to_oneinch_updates() -> Result<()> {
    let networks = [
        Network::Ethereum,
        Network::Polygon,
        Network::Bsc,
        Network::Arbitrum,
        Network::Base,
        Network::Gnosis,
    ];
    loop {
        for &network in &networks {
            if let Err(e) = update_oneinch_tokens(network).await {
                log::error!(
                    "Failed to update 1inch tokens for {}: {e}",
                    network.network_id_for_geckoterminal()
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}
