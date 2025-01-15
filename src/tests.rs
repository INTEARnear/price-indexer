use inindexer::near_indexer_primitives::types::AccountId;

use crate::{extract_pool_data, get_token_metadata, Tokens};

#[tokio::test]
async fn test_get_decimals() {
    assert_eq!(
        get_token_metadata("wrap.near".parse().unwrap())
            .await
            .unwrap()
            .decimals,
        24,
    );
    assert_eq!(
        get_token_metadata("usdt.tether-token.near".parse().unwrap())
            .await
            .unwrap()
            .symbol,
        "USDt",
    );
    assert_eq!(
        get_token_metadata("intel.tkn.near".parse().unwrap())
            .await
            .unwrap()
            .decimals,
        18,
    );
}

#[tokio::test]
async fn test_prices() {
    // TODO split this into multiple tests

    use intear_events::events::trade::trade_pool_change::*;

    let mut tokens = Tokens::new();
    assert!(!tokens
        .tokens
        .contains_key(&"wrap.near".parse::<AccountId>().unwrap()));

    // USDT pool
    let near_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "wrap.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![85_000e24 as u128, 450_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let near_usdt_data = extract_pool_data(&near_usdt_pool).unwrap();

    tokens
        .update_pool("REF-3879", near_usdt_pool, near_usdt_data)
        .await;
    assert_eq!(
        tokens
            .tokens
            .get(&"wrap.near".parse::<AccountId>().unwrap())
            .unwrap()
            .price_usd
            .with_prec(3)
            .to_string(),
        "5.29"
    );

    // NEAR pool
    let intel_near_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "wrap.near".parse().unwrap(),
        ],
        amounts: vec![21_000_000_000e18 as u128, 3_000e24 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_near_pool).unwrap();

    tokens
        .update_pool("REF-4663", intel_near_pool, intel_near_data)
        .await;
    assert_eq!(
        format!(
            "{}",
            tokens
                .tokens
                .get(&"intel.tkn.near".parse::<AccountId>().unwrap())
                .unwrap()
                .price_usd
        ),
        "7.563025210084033828572672741913708082991157956068469286035798430450272708894650889663257815808064710927543192587394838277468435669648407828308192740800792742453904723431559832838369480968618532007096000000000000E-7"
    );

    // Other token (intel) pool
    let chads_intel_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "chads.tkn.near".parse().unwrap(),
        ],
        amounts: vec![1_666_666_666e18 as u128, 1_666e18 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let chads_intel_data = extract_pool_data(&chads_intel_pool).unwrap();

    tokens
        .update_pool("REF-4774", chads_intel_pool, chads_intel_data)
        .await;
    assert_eq!(
        tokens
            .tokens
            .get(&"chads.tkn.near".parse::<AccountId>().unwrap())
            .unwrap()
            .price_usd
            .with_prec(3)
            .to_string(),
        "0.757"
    );

    // Update existing pool, lower token liquidity
    let chads_intel_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "chads.tkn.near".parse().unwrap(),
        ],
        amounts: vec![2_000_000_000e18 as u128, 1_500e18 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let chads_intel_data = extract_pool_data(&chads_intel_pool).unwrap();

    tokens
        .update_pool("REF-4774", chads_intel_pool, chads_intel_data)
        .await;
    assert_eq!(
        tokens
            .tokens
            .get(&"chads.tkn.near".parse::<AccountId>().unwrap())
            .unwrap()
            .price_usd
            .with_prec(3)
            .to_string(),
        "1.01"
    );

    // Add new pool with lower token liquidity, price shouldn't change
    let intel_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![20_000_000_000e18 as u128, 20_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_usdt_pool).unwrap();

    tokens
        .update_pool("TEST-69", intel_usdt_pool, intel_near_data)
        .await;
    assert_eq!(
        format!(
            "{}",
            tokens
                .tokens
                .get(&"intel.tkn.near".parse::<AccountId>().unwrap())
                .unwrap()
                .price_usd
        ),
        "7.563025210084033828572672741913708082991157956068469286035798430450272708894650889663257815808064710927543192587394838277468435669648407828308192740800792742453904723431559832838369480968618532007096000000000000E-7"
    );

    // Add new pool with higher token liquidity, price should change
    let intel_usdt_pool = PoolType::Ref(RefPool::SimplePool(RefSimplePool {
        token_account_ids: vec![
            "intel.tkn.near".parse().unwrap(),
            "usdt.tether-token.near".parse().unwrap(),
        ],
        amounts: vec![200_000_000_000e18 as u128, 200_000e6 as u128],
        volumes: vec![
            RefSwapVolume {
                input: 0,
                output: 0,
            },
            RefSwapVolume {
                input: 0,
                output: 0,
            },
        ],
        total_fee: 0,
        exchange_fee: 0,
        referral_fee: 0,
        shares_total_supply: 0,
    }));
    let intel_near_data = extract_pool_data(&intel_usdt_pool).unwrap();

    tokens
        .update_pool("TEST-42", intel_usdt_pool, intel_near_data)
        .await;
    assert_eq!(
        tokens
            .tokens
            .get(&"intel.tkn.near".parse::<AccountId>().unwrap())
            .unwrap()
            .price_usd
            .with_prec(3)
            .to_string(),
        "0.00000100"
    );
}
