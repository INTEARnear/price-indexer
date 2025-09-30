use std::env;

pub fn is_testnet() -> bool {
    env::var("TESTNET").is_ok()
}

pub fn get_usd_token() -> &'static str {
    if is_testnet() {
        "usdc.fakes.testnet"
    } else {
        "usdt.tether-token.near"
    }
}

pub fn get_usd_decimals() -> u32 {
    #[allow(clippy::if_same_then_else)]
    if is_testnet() {
        6
    } else {
        6
    }
}

pub fn get_usd_routes() -> &'static [(&'static str, &'static str)] {
    if is_testnet() {
        &[
            ("wrap.testnet", "REF-54"), // NEAR-USDC simple pool
        ]
    } else {
        &[
            ("wrap.near", "REF-6063"), // NEAR-USDt simple pool
            (
                "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
                "REF-6416",
            ), // USDt-USDC stable pool
        ]
    }
}

pub fn get_hardcoded_main_pool(token_id: &str) -> Option<&'static str> {
    match (is_testnet(), token_id) {
        (false, "wrap.near") => Some("REF-5470"),
        (false, "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1") => {
            Some("REF-4513")
        }
        (false, "nbtc.bridge.near") => Some("REF-5949"),
        _ => None,
    }
}
