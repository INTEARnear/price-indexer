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
            ("wrap.testnet", "REF-54"), // NEAR-USDC
        ]
    } else {
        &[
            ("wrap.near", "REF-3879"), // NEAR-USDt
        ]
    }
}
