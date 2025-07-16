use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use cached::proc_macro::io_cached;
use inindexer::near_indexer_primitives::types::{AccountId, Balance, BlockHeight};
use inindexer::near_utils::dec_format;
use intear_events::events::trade::trade_pool_change::PoolType;
use num_traits::cast::ToPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::price_sources::oneinch::{get_oneinch_price, Network};
use crate::token_metadata::TokenMetadataWithOptionalIcon;
use crate::utils::serde_bigdecimal;
use crate::{
    get_reqwest_client, network, pool_data::PoolData, price_sources::binance::get_binance_price,
    price_sources::jupiter::get_jupiter_price,
};

type GetTokenPriceFn = fn(&BigDecimal) -> BigDecimal;
const HARDCODED_TOKEN_PRICES: &[(&str, GetTokenPriceFn)] = &[
    // ("usdt.tether-token.near", stablecoin_price), // USDt is already always 1.00 since it's returned by get_usd_token()
    (
        "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1", // USDC
        stablecoin_price,
    ),
    (
        "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near", // USDT.e
        stablecoin_price,
    ),
    (
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near", // USDC.e
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
    pub total_supply: Balance,
    #[serde(with = "dec_format")]
    #[serde(default)]
    pub circulating_supply: Balance,
    #[serde(with = "dec_format")]
    #[serde(default)]
    pub circulating_supply_excluding_team: Balance,
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
            "bera.omft.near" => match get_binance_price("BERAUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "gnosis.omft.near" => HardcodedTokenPrice::Alias(
                "6b175474e89094c44da98b954eedeac495271d0f.factory.bridge.near"
                    .parse()
                    .unwrap(),
            ),
            "xrp.omft.near" => match get_binance_price("XRPUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "pol.omft.near" => match get_binance_price("POLUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "btc.omft.near" => HardcodedTokenPrice::Alias(
                "2260fac5e5542a773aa44fbcfedf7c193bc2c599.factory.bridge.near"
                    .parse()
                    .unwrap(),
            ),
            "sol.omft.near" => {
                HardcodedTokenPrice::Alias("22.contract.portalbridge.near".parse().unwrap())
            }
            "base.omft.near" => HardcodedTokenPrice::Alias("aurora".parse().unwrap()),
            "doge.omft.near" => match get_binance_price("DOGEUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "bsc.omft.near" => match get_binance_price("BNBUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "arb.omft.near" => match get_binance_price("ARBUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "zec.omft.near" => match get_binance_price("ZECUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "sui.omft.near" => match get_binance_price("SUIUSDT") {
                Some(price) => HardcodedTokenPrice::Price(price),
                None => HardcodedTokenPrice::TemporaryUnavailable,
            },
            "eth.omft.near" => HardcodedTokenPrice::Alias("aurora".parse().unwrap()),
            _ => match token_id.as_str().split_once('-') {
                Some(("sol", token_id)) => {
                    match get_jupiter_price(token_id.trim_end_matches(".omft.near"))
                        .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("eth", token_id)) => {
                    match get_oneinch_price(
                        Network::Ethereum,
                        token_id.trim_end_matches(".omft.near"),
                    )
                    .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("pol", token_id)) => {
                    match get_oneinch_price(
                        Network::Polygon,
                        token_id.trim_end_matches(".omft.near"),
                    )
                    .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("bsc", token_id)) => {
                    match get_oneinch_price(Network::Bsc, token_id.trim_end_matches(".omft.near"))
                        .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("gnosis", token_id)) => {
                    match get_oneinch_price(
                        Network::Gnosis,
                        token_id.trim_end_matches(".omft.near"),
                    )
                    .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("arb", token_id)) => {
                    match get_oneinch_price(
                        Network::Arbitrum,
                        token_id.trim_end_matches(".omft.near"),
                    )
                    .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                Some(("base", token_id)) => {
                    match get_oneinch_price(Network::Base, token_id.trim_end_matches(".omft.near"))
                        .map(FromPrimitive::from_f64)
                    {
                        Some(Some(price)) => HardcodedTokenPrice::Price(price),
                        _ => HardcodedTokenPrice::TemporaryUnavailable,
                    }
                }
                _ => HardcodedTokenPrice::TemporaryUnavailable,
            },
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
        | "horny.tkn.near"
        | "token.pembrock.near"
        | "edge-fast.near"
        | "deezz.near"
        | "hat.tkn.near"
        | "meta-token.near"
        | "lockedjumptoken.jumpfinance.near"
        | "4e807467ba9e3119d5356c5568ef63e9c321b471.factory.bridge.near"
        | "pixeltoken.near"
        | "a663b02cf0a4b149d2ad41910cb81e23e1c41c32.factory.bridge.near"
        | "2260fac5e5542a773aa44fbcfedf7c193bc2c599.factory.bridge.near"
        | "token.skyward.near"
        | "neat.nrc-20.near"
        | "xjumptoken.jumpfinance.near"
        | "ft.zomland.near"
        | "nkok.tkn.near"
        | "dragonsoultoken.near"
        | "touched.tkn.near"
        | "bgn.tkn.near"
        | "babyblackdragon.tkn.near"
        | "438e48ed4ce6beecf503d43b9dbd3c30d516e7fd.factory.bridge.near"
        | "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2.factory.bridge.near"
        | "sol.token.a11bd.near"
        | "f5cfbc74057c610c8ef151a439252680ac68c6dc.factory.bridge.near"
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
        | "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near"
        | "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near"
        | "token.burrow.near"
        | "token.v2.ref-finance.near"
        | "intel.tkn.near"
        | "6b175474e89094c44da98b954eedeac495271d0f.factory.bridge.near"
        | "111111111117dc0aa78b770fa6a738034120c302.factory.bridge.near"
        | "wbnb.hot.tg"
        | "aptos-88cb7619440a914fe6400149a12b443c3ac21d59.omft.near"
        | "aptos.omft.near"
        | "arb-0x912ce59144191c1204e64559fe8253a0e49e6548.omft.near"
        | "arb-0xaf88d065e77c8cc2239327c5edb3a432268e5831.omft.near"
        | "arb-0xca7dec8550f43a5e46e3dfb95801f64280e75b27.omft.near"
        | "arb-0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a.omft.near"
        | "arb-0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9.omft.near"
        | "arb.omft.near"
        | "base-0x227d920e20ebac8a40e7d6431b7d724bb64d7245.omft.near"
        | "base-0x532f27101965dd16442e59d40670faf5ebb142e4.omft.near"
        | "base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near"
        | "base-0x98d0baa52b2d063e780de12f615f963fe8537553.omft.near"
        | "base-0xa5c67d8d37b88c2d88647814da5578128e2c93b2.omft.near"
        | "base-0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf.omft.near"
        | "base.omft.near"
        | "bera.omft.near"
        | "btc.omft.near"
        | "doge.omft.near"
        | "eth-0x1f9840a85d5af5bf1d1762f925bdaddc4201f984.omft.near"
        | "eth-0x2260fac5e5542a773aa44fbcfedf7c193bc2c599.omft.near"
        | "eth-0x514910771af9ca656af840dff83e8264ecf986ca.omft.near"
        | "eth-0x6982508145454ce325ddbe47a25d4ec3d2311933.omft.near"
        | "eth-0x6b175474e89094c44da98b954eedeac495271d0f.omft.near"
        | "eth-0x7fc66500c84a76ad7e9c93437bfc5ac33e2ddae9.omft.near"
        | "eth-0x8d0d000ee44948fc98c9b98a4fa4921476f08b0d.omft.near"
        | "eth-0x95ad61b0a150d79219dcf64e1e6cc01f0b64c4ce.omft.near"
        | "eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near"
        | "eth-0xa35923162c49cf95e6bf26623385eb431ad920d3.omft.near"
        | "eth-0xaaaaaa20d9e0e2461697782ef11675f668207961.omft.near"
        | "eth-0xaaee1a9723aadb7afa2810263653a34ba2c21c7a.omft.near"
        | "eth-0xb4b9dc1c77bdbb135ea907fd5a08094d98883a35.omft.near"
        | "eth-0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf.omft.near"
        | "eth-0xd9c2d319cd7e6177336b0a9c93c21cb48d84fb54.omft.near"
        | "eth-0xdac17f958d2ee523a2206206994597c13d831ec7.omft.near"
        | "eth-0xdefa4e8a7bcba345f687a2f1456f5edd9ce97202.omft.near"
        | "eth-0xfa2b947eec368f42195f24f36d2af29f7c24cec2.omft.near"
        | "eth.bridge.near"
        | "eth.omft.near"
        | "gnosis-0x177127622c4a00f3d409b75571e12cb3c8973d3c.omft.near"
        | "gnosis-0x2a22f9c3b484c3629090feed35f17ff8f88f76f0.omft.near"
        | "gnosis-0x4d18815d14fe5c3304e87b3fa18318baa5c23820.omft.near"
        | "gnosis-0x6a023ccd1ff6f2045c3309768ead9e68f978f6e1.omft.near"
        | "gnosis-0x9c58bacc331c9aa871afd802db6379a98e80cedb.omft.near"
        | "gnosis.omft.near"
        | "sol-57d087fd8c460f612f8701f5499ad8b2eec5ab68.omft.near"
        | "sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near"
        | "sol-91914f13d3b54f8126a2824d71632d4b078d7403.omft.near"
        | "sol-b9c68f94ec8fd160137af8cdfe5e61cd68e2afba.omft.near"
        | "sol-bb27241c87aa401cc963c360c175dd7ca7035873.omft.near"
        | "sol-c58e6539c2f2e097c251f8edf11f9c03e581f8d4.omft.near"
        | "sol-c800a4bd850783ccb82c2b2c7e84175443606352.omft.near"
        | "sol-d600e625449a4d9380eaf5e3265e54c90d34e260.omft.near"
        | "sol-df27d7abcc1c656d4ac3b1399bbfbba1994e6d8c.omft.near"
        | "sol.omft.near"
        | "sui-349a5b23674603c086ceac1fa9f139c4bbc30cf8.omft.near"
        | "sui-c1b81ecaf27933252d31a963bc5e9458f13c18ce.omft.near"
        | "sui.omft.near"
        | "tron-d28a265909efecdcee7c5028585214ea0b96f015.omft.near"
        | "tron.omft.near"
        | "xrp.omft.near"
        | "zec.omft.near"
        | "wojak.tkn.near" => TokenScore::NotFake,
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
        | "802d89b6e511b335f05024a65161bce7efc3f311.factory.bridge.near"
        | "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"
        | "514910771af9ca656af840dff83e8264ecf986ca.factory.bridge.near"
        | "853d955acef822db058eb8505911ed77f175b99e.factory.bridge.near"
        | "aaaaaa20d9e0e2461697782ef11675f668207961.factory.bridge.near"
        | "nbtc.bridge.near"
        | "token.paras.near" => TokenScore::Reputable,
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
        "dac17f958d2ee523a2206206994597c13d831ec7.factory.bridge.near" => vec!["usdte", "usdt"],
        "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.factory.bridge.near" => vec!["usdce", "usdc"],
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
            "model": "claude-3-5-sonnet-20240620",
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
