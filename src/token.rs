use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::Duration;

use cached::proc_macro::io_cached;
use inindexer::near_indexer_primitives::types::{AccountId, BlockHeight};
use inindexer::near_utils::{dec_format, FtBalance};
use intear_events::events::trade::trade_pool_change::PoolType;
use num_traits::cast::ToPrimitive;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::token_metadata::TokenMetadataWithOptionalIcon;
use crate::utils::serde_bigdecimal;
use crate::{get_reqwest_client, network, pool_data::PoolData};

type GetTokenPriceFn = fn(&BigDecimal) -> BigDecimal;
const HARDCODED_TOKEN_PRICES: &[(&str, GetTokenPriceFn)] = &[
    // ("usdt.tether-token.near", stablecoin_price), // USDt is already always 1.00 since it's returned by get_usd_token()
    (
        "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1", // USDC
        stablecoin_price,
    ),
    (
        "853d955acef822db058eb8505911ed77f175b99e.factory.bridge.near", // FRAX
        stablecoin_price,
    ),
    ("pre.meteor-token.near", |_| 0.into()), // MEPT
];

fn stablecoin_price(actual_price_usd: &BigDecimal) -> BigDecimal {
    const STABLECOIN_BASE_PRICE_USD: f64 = 1.0;
    const STABLECOIN_MAX_DIFFERENCE_USD: f64 = 0.01;

    let price = actual_price_usd.clone();
    let price_f64 = f64::from_str(&price.to_string()).unwrap();
    if (price_f64 - STABLECOIN_BASE_PRICE_USD).abs() < STABLECOIN_MAX_DIFFERENCE_USD {
        1.into()
    } else {
        price
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(default = "default_account_id")]
    pub account_id: AccountId,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd_raw: BigDecimal,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd: BigDecimal,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd_hardcoded: BigDecimal,
    #[serde(with = "serde_bigdecimal", default)]
    pub price_usd_raw_24h_ago: BigDecimal,
    /// 'Main pool' is a pool that leads to a token that can be farther converted
    /// into [`USD_TOKEN`] through one of [`USD_ROUTES`].
    pub main_pool: Option<String>,
    pub metadata: TokenMetadataWithOptionalIcon,
    #[serde(with = "dec_format")]
    #[serde(default)]
    pub total_supply: FtBalance,
    #[serde(with = "dec_format")]
    #[serde(default)]
    pub circulating_supply: FtBalance,
    #[serde(with = "dec_format")]
    #[serde(default)]
    pub circulating_supply_excluding_team: FtBalance,
    #[serde(default, skip_deserializing)]
    pub reputation: TokenScore,
    #[serde(default, skip_deserializing)]
    pub socials: HashMap<String, String>,
    #[serde(default, skip_deserializing)]
    pub slug: Vec<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub reference: serde_json::Value,
    #[serde(default)]
    pub liquidity_usd: f64,
    #[serde(default)]
    pub volume_usd_24h: f64,
    #[serde(default)]
    pub created_at: BlockHeight,
}

fn default_account_id() -> AccountId {
    "0".repeat(64).parse().unwrap()
}

impl Token {
    pub fn sorting_score(&self, search: &str) -> u128 {
        if (self.account_id == "wrap.near" || self.account_id == "wrap.testnet")
            && ("near".starts_with(search)
                || "wnear".starts_with(search)
                || "wrap.near".starts_with(search)
                || "wrap.testnet".starts_with(search))
        {
            return 69696969696969;
        }
        let relevancy = if search.trim_start_matches('$')
            == self.metadata.name.to_lowercase().trim_start_matches('$')
            || search.trim_start_matches('$')
                == self.metadata.symbol.to_lowercase().trim_start_matches('$')
        {
            1000
        } else if search == self.account_id || self.slug.contains(&search.to_owned()) {
            900
        } else if self.metadata.symbol.to_lowercase().starts_with(search)
            || self.metadata.name.to_lowercase().starts_with(search)
            || self.slug.iter().any(|slug| slug.starts_with(search))
        {
            300
        } else if self.metadata.symbol.to_lowercase().contains(search)
            || self.metadata.name.to_lowercase().contains(search)
            || self.account_id.as_str().contains(search)
            || self.slug.iter().any(|slug| slug.contains(search))
        {
            30
        } else if self.socials.values().any(|v| v == search) {
            20
        } else if self.socials.values().any(|v| v.contains(search)) {
            5
        } else {
            0
        };
        let reputation_score = match self.reputation {
            TokenScore::Spam => 1,
            TokenScore::Unknown => 10,
            TokenScore::NotFake => 100,
            TokenScore::Reputable => 150,
        };
        let circulating_supply_human_readable =
            self.circulating_supply_excluding_team / 10u128.pow(self.metadata.decimals);
        let market_cap_usd = self.price_usd_hardcoded.to_f64().unwrap_or_default()
            * circulating_supply_human_readable as f64;
        let market_cap_score = if market_cap_usd < 1_000.0 {
            50
        } else if market_cap_usd < 10_000.0 {
            75
        } else if market_cap_usd < 100_000.0 {
            100
        } else if market_cap_usd < 1_000_000.0 {
            120
        } else if market_cap_usd < 10_000_000.0 {
            150
        } else {
            200
        };
        // TODO also include volume in calculation
        relevancy * reputation_score * market_cap_score
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord, Default, Copy)]
pub enum TokenScore {
    Spam,
    #[default]
    Unknown,
    NotFake,
    Reputable,
}

pub fn calculate_price(
    token_id: &AccountId,
    pools: &HashMap<String, (PoolType, PoolData)>,
    routes: &HashMap<AccountId, String>,
    pool: &str,
) -> BigDecimal {
    if token_id == network::get_usd_token() {
        return BigDecimal::from(1);
    }

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
        if current_token_id == network::get_usd_token() {
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
            log::error!("USD route not found for {current_token_id} (for {token_id})");
            break BigDecimal::from(0);
        }
    }
}

pub enum HardcodedTokenPrice {
    #[allow(dead_code)]
    /// If not available, use the previous price.
    TemporaryUnavailable,
    Price(BigDecimal),
    /// Use this token's price instead
    Alias(AccountId),
}

pub fn get_hardcoded_price_usd(
    token_id: &AccountId,
    actual_price_usd: &BigDecimal,
) -> HardcodedTokenPrice {
    if let Some(hardcoded_price_fn) = HARDCODED_TOKEN_PRICES
        .iter()
        .find(|(hardcoded_token_id, _)| hardcoded_token_id == token_id)
        .map(|(_, price_fn)| price_fn)
    {
        HardcodedTokenPrice::Price(hardcoded_price_fn(actual_price_usd))
    } else if token_id.as_str().ends_with(".omft.near") {
        match token_id.as_str() {
            "btc.omft.near" => HardcodedTokenPrice::Alias(
                "2260fac5e5542a773aa44fbcfedf7c193bc2c599.factory.bridge.near"
                    .parse()
                    .unwrap(),
            ),
            "sol.omft.near" => {
                HardcodedTokenPrice::Alias("22.contract.portalbridge.near".parse().unwrap())
            }
            "base.omft.near" => HardcodedTokenPrice::Alias("eth.bridge.near".parse().unwrap()),
            "eth.omft.near" => HardcodedTokenPrice::Alias("eth.bridge.near".parse().unwrap()),
            _ => HardcodedTokenPrice::Price(actual_price_usd.clone()),
        }
    } else {
        HardcodedTokenPrice::Price(actual_price_usd.clone())
    }
}

/// Evaluation criteria:
///
/// - `Spam` - tokens that are sent to many users without their consent to advertise something
///   (usually scams)
/// - `Unknown` - tokens that are not in any of the other categories, this is the default value
///   for new tokens that are not categorized yet
/// - `NotFake` - tokens that are real, used to differentiate between real tokens and scams
///   pretending to be them. Any project can add themselves to this list, even if it's a shitcoin
///   that launched 10 minutes ago and has 2 people in their community, as long as there is no
///   other token with the same name or symbol and the name is not misleading.
/// - `Reputable` - tokens that are well-known and have a good reputation. The rule of thumb is
///   that the project is highly unlikely to disappear in the next year. This list is subjective,
///   the key factors are the project's age, the size and noise of the community, quality of the
///   project, whether the team is anonymous or it's backed by OGs, has good liquidity, utility,
///   and so on. Tokens that are owned by reputable projects but are not tradable, have low
///   liquidity, or otherwise not expected to be searched by users often (xREF, LJUMP, etc.), are
///   considered NotFake.
pub fn get_reputation(token_id: &AccountId, spam_tokens: &HashSet<AccountId>) -> TokenScore {
    match token_id.as_str() {
        "usn"
        | "utopia.secretskelliessociety.near"
        | "slush.tkn.near"
        | "token.pumpopoly.near"
        | "usmeme.tg"
        | "stop.tkn.near"
        | "nearnvidia.near"
        | "pussy.laboratory.jumpfinance.near"
        | "v1.dacha-finance.near"
        | "token.cheddar.near"
        | "avb.tkn.near"
        | "phoenix-bonds.near"
        | "ndc.tkn.near"
        | "coin.asac.near"
        | "chads.tkn.near"
        | "myriadcore.near"
        | "xtoken.ref-finance.near"
        | "otoken.ref-finance.near"
        | "horny.tkn.near"
        | "token.pembrock.near"
        | "edge-fast.near"
        | "deezz.near"
        | "hat.tkn.near"
        | "meta-token.near"
        | "lockedjumptoken.jumpfinance.near"
        | "pixeltoken.near"
        | "token.skyward.near"
        | "neat.nrc-20.near"
        | "xjumptoken.jumpfinance.near"
        | "ft.zomland.near"
        | "nkok.tkn.near"
        | "dragonsoultoken.near"
        | "touched.tkn.near"
        | "bgn.tkn.near"
        | "babyblackdragon.tkn.near"
        | "sol.token.a11bd.near"
        | "bean.tkn.near"
        | "pre.meteor-token.near"
        | "rugrace.tkn.near"
        | "dd.tg"
        | "poppy-0.meme-cooking-test.near"
        | "bulla.tkn.near"
        | "sin-339.meme-cooking.near"
        | "pumpkg-332.meme-cooking.near"
        | "noear-324.meme-cooking.near"
        | "nearvember-337.meme-cooking.near"
        | "massive-260.meme-cooking.near"
        | "hijack-252.meme-cooking.near"
        | "6bowen-227.meme-cooking.near"
        | "gnear-229.meme-cooking.near"
        | "4illia-222.meme-cooking.near"
        | "neardog-0.meme-cooking.near"
        | "redacted-172.meme-cooking.near"
        | "chill-129.meme-cooking.near"
        | "purge-558.meme-cooking.near"
        | "rin.tkn.near"
        | "abg-966.meme-cooking.near"
        | "bullish-1254.meme-cooking.near"
        | "jlu-1018.meme-cooking.near"
        | "duct-1078.meme-cooking.near"
        | "benthedog.near"
        | "aurora"
        | "token.burrow.near"
        | "token.v2.ref-finance.near"
        | "intel.tkn.near"
        | "wbnb.hot.tg"
        | "eth.bridge.near"
        | "zec.omft.near"
        | "kat.token0.near"
        | "token.paras.near"
        | "jambo-1679.meme-cooking.near"
        | "zolanear-1726.meme-cooking.near"
        | "vote-1737.meme-cooking.near"
        | "853d955acef822db058eb8505911ed77f175b99e.factory.bridge.near" => TokenScore::NotFake,
        "token.lonkingnearbackto2024.near"
        | "token.sweat"
        | "meta-pool.near"
        | "token.intear.near"
        | "usdt.tether-token.near"
        | "gear.enleap.near"
        | "linear-protocol.near"
        | "marmaj.tkn.near"
        | "blackdragon.tkn.near"
        | "jumptoken.jumpfinance.near"
        | "ftv2.nekotoken.near"
        | "token.0xshitzu.near"
        | "mpdao-token.near"
        | "wrap.near"
        | "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"
        | "nbtc.bridge.near"
        | "token.rhealab.near"
        | "xtoken.rhealab.near"
        | "lst.rhealab.near"
        | "token.publicailab.near" => TokenScore::Reputable,
        _ if spam_tokens.contains(token_id) => TokenScore::Spam,
        _ => TokenScore::Unknown,
    }
}

pub fn get_slug(token_id: &AccountId) -> Vec<&'static str> {
    match token_id.as_str() {
        "blackdragon.tkn.near" => vec!["bd"],
        "babyblackdragon.tkn.near" => vec!["bbd"],
        "token.sweat" => vec!["sweatcoin"],
        "jumptoken.jumpfinance.near" => vec!["jumpdefi"],
        "mpdao-token.near" => vec!["metapool", "meta pool"],
        _ => Vec::new(),
    }
}

pub fn get_socials(token_id: &AccountId) -> HashMap<&'static str, &'static str> {
    const TWITTER: &str = "twitter";
    const TELEGRAM: &str = "telegram";
    const WEBSITE: &str = "website";

    match token_id.as_str() {
        "intel.tkn.near" => HashMap::from_iter([
            (TWITTER, "https://x.com/intelnear"),
            (TELEGRAM, "https://t.me/intearchat"),
            (WEBSITE, "https://intear.tech"),
        ]),
        // TODO
        _ => HashMap::new(),
    }
}

#[io_cached(
    time = 100000000000000000,
    disk = true,
    map_error = "|e| anyhow::anyhow!(e)"
)]
pub async fn is_spam(s: &str) -> Result<bool, anyhow::Error> {
    log::info!("Checking for spam:\n{s}");
    let system_message = "A new cryptocurrency token was created on the NEAR blockchain with the details given.

If the token details contain links, or otherwise appear to be spam that is massively sent out to millions of users, reply with (Y). If the token details do not contain any promotion, reply with (N). Even if the token name or symbol may appear inappropriate or sound like a scam token, as long as it doesn't contain a link or advertisement, reply with (N). Reply with nothing else other than (Y) or (N).".to_string();
    let user_message = s;
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY env var is not set");

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)] // Used in Debug implementation
    struct Response {
        id: String,
        r#type: String,
        role: String,
        model: String,
        content: Vec<ResponseContent>,
        stop_reason: String,
        usage: Usage,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct ResponseContent {
        r#type: String,
        text: String,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Usage {
        input_tokens: u64,
        output_tokens: u64,
    }
    let response: serde_json::Value = get_reqwest_client()
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-5-20250929",
            "max_tokens": 6,
            "system": system_message,
            "messages": [
                {"role": "user", "content": user_message},
            ]
        }))
        .send()
        .await?
        .json()
        .await?;
    let response_debug = format!("{response:?}");
    match serde_json::from_value::<Response>(response) {
        Ok(response) => {
            let is_spam = response
                .content
                .iter()
                .any(|content| content.text.contains('Y'));
            log::info!(
                "{}",
                if is_spam {
                    "Flagging as spam"
                } else {
                    "Not spam"
                }
            );
            Ok(is_spam)
        }
        Err(e) => {
            log::error!("Failed to parse response {response_debug}: {e}");
            Err(anyhow::anyhow!("Failed to parse response"))
        }
    }
}
