use futures_util::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};
use tokio_util::sync::CancellationToken;

lazy_static! {
    static ref BINANCE_PRICES: Arc<RwLock<HashMap<String, BigDecimal>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

#[derive(Debug, Serialize, Deserialize)]
struct BinanceStreamData {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "c")]
    close_price: String,
}

pub fn get_binance_price(symbol: &str) -> Option<BigDecimal> {
    BINANCE_PRICES.read().get(symbol).cloned()
}

async fn connect_to_binance_ws(url: &str, symbols: &[String]) -> Result<(), anyhow::Error> {
    let request = url.into_client_request()?;
    let (ws_stream, _) = connect_async(request).await?;
    let (mut write, mut read) = ws_stream.split();

    let subscribe_msg = serde_json::json!({
        "method": "SUBSCRIBE",
        "params": symbols.iter().map(|s| format!("{}@ticker", s.to_lowercase())).collect::<Vec<_>>(),
        "id": 1
    });
    write
        .send(Message::Text(subscribe_msg.to_string().into()))
        .await?;

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Ok(data) = serde_json::from_str::<BinanceStreamData>(&text) {
                    if let Ok(price) = data.close_price.parse() {
                        BINANCE_PRICES.write().insert(data.symbol, price);
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                write.send(Message::Pong(data)).await?;
            }
            Err(e) => {
                log::error!("Error in Binance WebSocket stream: {e}");
                return Err(e.into());
            }
            _ => {}
        }
    }

    Ok(())
}

pub async fn start_binance_ws(cancellation_token: CancellationToken) {
    let symbols = [
        "BERAUSDT", "XRPUSDT", "POLUSDT", "BTCUSDT", "DOGEUSDT", "BNBUSDT", "ARBUSDT", "ZECUSDT",
    ];

    let streams = symbols
        .iter()
        .map(|s| format!("{}@ticker", s.to_lowercase()))
        .collect::<Vec<_>>()
        .join("/");

    let url = format!("wss://stream.binance.com:9443/ws/{streams}");
    let symbols: Vec<String> = symbols.iter().map(|s| s.to_string()).collect();

    let mut retry_delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(300);

    loop {
        if cancellation_token.is_cancelled() {
            break;
        }

        log::info!("Connecting to Binance WebSocket...");

        match connect_to_binance_ws(&url, &symbols).await {
            Ok(_) => {
                retry_delay = Duration::from_secs(1);
                log::info!("WebSocket connection closed normally, reconnecting...");
            }
            Err(e) => {
                log::error!("WebSocket connection error: {}", e);
                log::info!("Reconnecting in {} seconds...", retry_delay.as_secs());
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(retry_delay) => {}
            _ = cancellation_token.cancelled() => break,
        }
        retry_delay = std::cmp::min(retry_delay * 2, max_delay);
    }
}
