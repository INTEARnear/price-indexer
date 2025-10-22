use std::{
    collections::HashMap,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    network::is_testnet,
    token_metadata::get_token_metadata,
    utils::{get_rpc_url, serde_bigdecimal_tuple2},
};
use cached::proc_macro::cached;
use inindexer::{
    near_indexer_primitives::{
        types::{AccountId, Balance, BlockHeight, BlockId, BlockReference, Finality},
        views::QueryRequest,
    },
    near_utils::dec_format,
};
use intear_events::events::trade::trade_pool_change::{PoolType, RefPool};
use near_jsonrpc_client::{methods, JsonRpcClient};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use num_traits::{FromPrimitive, One, ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

#[cached(time = 1, result = true)]
pub async fn get_degens(
    block_height: Option<BlockHeight>,
) -> Result<HashMap<AccountId, Balance>, anyhow::Error> {
    let client = JsonRpcClient::connect(get_rpc_url());
    let request = methods::query::RpcQueryRequest {
        block_reference: if let Some(block_height) = block_height {
            BlockReference::BlockId(BlockId::Height(block_height))
        } else {
            BlockReference::Finality(Finality::None)
        },
        request: QueryRequest::CallFunction {
            account_id: "v2.ref-finance.near".parse().unwrap(),
            method_name: "list_degen_tokens".into(),
            args: serde_json::to_vec(&serde_json::json!({})).unwrap().into(),
        },
    };

    #[derive(Deserialize, Debug)]
    struct DegenTokenInfo {
        #[serde(with = "dec_format")]
        pub degen_price: Balance,
    }

    let response = client.call(request).await?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    let call_result: HashMap<AccountId, DegenTokenInfo> =
        serde_json::from_slice(&call_result.result)?;
    Ok(call_result
        .into_iter()
        .map(|(token_id, info)| (token_id, info.degen_price))
        .collect())
}

#[cached(time = 1, result = true)]
pub async fn get_rates(
    block_height: Option<BlockHeight>,
) -> Result<HashMap<AccountId, Balance>, anyhow::Error> {
    let client = JsonRpcClient::connect(get_rpc_url());
    let request = methods::query::RpcQueryRequest {
        block_reference: if let Some(block_height) = block_height {
            BlockReference::BlockId(BlockId::Height(block_height))
        } else {
            BlockReference::Finality(Finality::None)
        },
        request: QueryRequest::CallFunction {
            account_id: "v2.ref-finance.near".parse().unwrap(),
            method_name: "list_rated_tokens".into(),
            args: serde_json::to_vec(&serde_json::json!({})).unwrap().into(),
        },
    };

    #[derive(Deserialize, Debug)]
    struct RatedTokenInfo {
        #[serde(with = "dec_format")]
        pub rate_price: Balance,
    }

    let response = client.call(request).await?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    let call_result: HashMap<AccountId, RatedTokenInfo> =
        serde_json::from_slice(&call_result.result)?;
    Ok(call_result
        .into_iter()
        .map(|(token_id, info)| (token_id, info.rate_price))
        .collect())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PoolData {
    pub tokens: (AccountId, AccountId),
    #[serde(with = "serde_bigdecimal_tuple2")]
    pub ratios: (BigDecimal, BigDecimal),
    #[serde(with = "serde_bigdecimal_tuple2")]
    pub liquidity: (BigDecimal, BigDecimal),
}

pub async fn extract_pool_data(
    pool: &PoolType,
    block_height: Option<BlockHeight>,
) -> Option<PoolData> {
    match pool {
        PoolType::Ref(pool) => {
            // Code below is copied from ref-finance/ref-contracts
            let stableswap_compute_amp_factor =
                move |init_amp_factor: u128,
                      target_amp_factor: u128,
                      init_amp_time: u128,
                      stop_amp_time: u128| {
                    let current_ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos();
                    if current_ts < stop_amp_time {
                        let time_range = stop_amp_time.checked_sub(init_amp_time)?;
                        let time_delta = current_ts.checked_sub(init_amp_time)?;

                        // Compute amp factor based on ramp time
                        if target_amp_factor >= init_amp_factor {
                            // Ramp up
                            let amp_range = target_amp_factor.checked_sub(init_amp_factor)?;
                            let amp_delta =
                                amp_range.checked_mul(time_delta)?.checked_div(time_range)?;
                            init_amp_factor.checked_add(amp_delta)
                        } else {
                            // Ramp down
                            let amp_range = init_amp_factor.checked_sub(target_amp_factor)?;
                            let amp_delta =
                                amp_range.checked_mul(time_delta)?.checked_div(time_range)?;
                            init_amp_factor.checked_sub(amp_delta)
                        }
                    } else {
                        // when stop_ramp_ts == 0 or current_ts >= stop_ramp_ts
                        Some(target_amp_factor)
                    }
                };

            // Compute stable swap invariant (D)
            // Equation:
            // A * sum(x_i) * n**n + D = A * D * n**n + D**(n+1) / (n**n * prod(x_i))
            let stableswap_compute_d =
                move |c_amounts: &Vec<BigDecimal>, amp_factor: u128| -> Option<BigDecimal> {
                    let n_coins = BigDecimal::from_u128(c_amounts.len() as u128)?;
                    let sum_x = c_amounts.iter().fold(BigDecimal::zero(), |sum, i| sum + i);
                    if sum_x == BigDecimal::zero() {
                        Some(BigDecimal::zero())
                    } else {
                        let mut d_prev: BigDecimal;
                        let mut d: BigDecimal = sum_x.clone();
                        for _ in 0..256 {
                            // $ D_{k,prod} = \frac{D_k^{n+1}}{n^n \prod x_{i}} = \frac{D^3}{4xy} $
                            let mut d_prod = d.clone();
                            for c_amount in c_amounts.iter() {
                                d_prod = d_prod * &d / (c_amount * &n_coins);
                            }
                            d_prev = d.clone();

                            let n_coins_u128 = n_coins.to_u128()?;
                            let ann = BigDecimal::from_u128(
                                amp_factor * n_coins_u128.pow(n_coins_u128 as u32),
                            )?;
                            let leverage = &sum_x * &ann;

                            // d = (ann * sum_x + d_prod * n_coins) * d_prev / ((ann - 1) * d_prev + (n_coins + 1) * d_prod)
                            let numerator = &d_prev * (&d_prod * &n_coins + &leverage);
                            let denominator = &d_prev * (&ann - BigDecimal::one())
                                + &d_prod * (&n_coins + BigDecimal::one());
                            d = numerator / denominator;

                            // Equality with the precision of 0.000001
                            let diff = if d > d_prev {
                                &d - &d_prev
                            } else {
                                &d_prev - &d
                            };
                            if diff <= BigDecimal::one() / BigDecimal::from_u128(10u128.pow(6))? {
                                break;
                            }
                        }
                        Some(d)
                    }
                };

            // Compute new amount of token 'y' with new amount of token 'x'
            // return new y_token amount according to the equation
            let stableswap_compute_y = move |x_c_amount: BigDecimal, // new x_token amount in comparable precision,
                                             current_c_amounts: &Vec<BigDecimal>, // in-pool tokens amount in comparable precision,
                                             index_x: usize,                      // x token's index
                                             index_y: usize,                      // y token's index
                                             amp_factor: u128|
                  -> Option<BigDecimal> {
                let n_coins = BigDecimal::from_u128(current_c_amounts.len() as u128)?;
                let n_coins_u128 = n_coins.to_u128()?;
                let ann =
                    BigDecimal::from_u128(amp_factor * n_coins_u128.pow(n_coins_u128 as u32))?;

                // invariant
                let d = stableswap_compute_d(current_c_amounts, amp_factor)?;
                let mut s_ = x_c_amount.clone();
                let mut c = &d * &d / x_c_amount;

                for (idx, c_amount) in current_c_amounts.iter().enumerate() {
                    if idx != index_x && idx != index_y {
                        s_ += c_amount;
                        c = c * &d / c_amount;
                    }
                }
                c = c * &d / (&ann * BigDecimal::from_u128(n_coins_u128.pow(n_coins_u128 as u32))?);

                let b = &d / &ann + &s_; // d will be subtracted later

                // Solve for y by approximating: y**2 + b*y = c
                let mut y_prev: BigDecimal;
                let mut y = d.clone();
                for _ in 0..256 {
                    y_prev = y.clone();
                    // $ y_{k+1} = \frac{y_k^2 + c}{2y_k + b - D} $
                    let y_numerator = &y * &y + &c;
                    let y_denominator = &y * BigDecimal::from_u128(2)? + &b - &d;
                    y = y_numerator / y_denominator;

                    let diff = if y > y_prev {
                        &y - &y_prev
                    } else {
                        &y_prev - &y
                    };
                    if diff <= BigDecimal::one() {
                        break;
                    }
                }
                Some(y)
            };
            match pool {
                RefPool::SimplePool(pool) => {
                    let (Ok([amount0, amount1]), Ok([token0, token1])) = (
                        <[u128; 2]>::try_from(pool.amounts.clone()),
                        <[AccountId; 2]>::try_from(pool.token_account_ids.clone()),
                    ) else {
                        return None;
                    };
                    let amount0 = BigDecimal::from_str(&amount0.to_string()).ok()?;
                    let amount1 = BigDecimal::from_str(&amount1.to_string()).ok()?;

                    if amount0 == 0.into() || amount1 == 0.into() {
                        return None;
                    }
                    let token0_in_1_token1 = amount0.clone() / amount1.clone();
                    let token1_in_1_token0 = amount1.clone() / amount0.clone();
                    Some(PoolData {
                        tokens: (token0, token1),
                        ratios: (token0_in_1_token1, token1_in_1_token0),
                        liquidity: (amount0, amount1),
                    })
                }
                RefPool::StableSwapPool(pool) => {
                    if pool.token_account_ids.len() != 2 {
                        return None;
                    }

                    let (token0_metadata, token1_metadata) = tokio::join!(
                        get_token_metadata(pool.token_account_ids[0].clone(), block_height),
                        get_token_metadata(pool.token_account_ids[1].clone(), block_height),
                    );
                    let decimals = [
                        token0_metadata.ok()?.decimals,
                        token1_metadata.ok()?.decimals,
                    ];

                    let ratios = [(1, 0), (0, 1)]
                        .iter()
                        .map(|&(token_in_idx, token_out_idx)| {
                            let token_in_amount = 1;

                            let new_x_amount = token_in_amount + pool.c_amounts[token_in_idx];

                            let y = stableswap_compute_y(
                                BigDecimal::from_u128(new_x_amount)?,
                                &pool
                                    .c_amounts
                                    .iter()
                                    .map(|x| BigDecimal::from_u128(*x).unwrap())
                                    .collect::<Vec<_>>(),
                                token_in_idx,
                                token_out_idx,
                                stableswap_compute_amp_factor(
                                    pool.init_amp_factor,
                                    pool.target_amp_factor,
                                    pool.init_amp_time as u128,
                                    pool.stop_amp_time as u128,
                                )?,
                            )?;

                            let current_y = pool.c_amounts[token_out_idx];
                            let dy = if y > BigDecimal::from_u128(current_y)? {
                                BigDecimal::zero()
                            } else {
                                BigDecimal::from_u128(current_y)? - &y
                            };

                            Some(
                                dy / BigDecimal::from_u128(10u128.pow(decimals[token_in_idx]))?
                                    * BigDecimal::from_u128(10u128.pow(decimals[token_out_idx]))?,
                            )
                        })
                        .collect::<Option<Vec<_>>>()?;

                    Some(PoolData {
                        tokens: (
                            pool.token_account_ids[0].clone(),
                            pool.token_account_ids[1].clone(),
                        ),
                        ratios: (ratios[0].clone(), ratios[1].clone()),
                        liquidity: (pool.c_amounts[0].into(), pool.c_amounts[1].into()),
                    })
                }
                RefPool::RatedSwapPool(pool) => {
                    if pool.token_account_ids.len() != 2 {
                        return None;
                    }

                    let precision = BigDecimal::from_u128(10u128.pow(24))?;

                    let mul_rated = |amount: Balance, rate: Balance| -> Option<BigDecimal> {
                        let amount_bd = BigDecimal::from_u128(amount)?;
                        let rate_bd = BigDecimal::from_u128(rate)?;
                        Some(amount_bd * rate_bd / &precision)
                    };

                    let div_rated = |amount: &BigDecimal, rate: Balance| -> Option<BigDecimal> {
                        let rate_bd = BigDecimal::from_u128(rate)?;
                        Some(amount * &precision / rate_bd)
                    };

                    let (token0_metadata, token1_metadata, rates) = tokio::join!(
                        get_token_metadata(pool.token_account_ids[0].clone(), block_height),
                        get_token_metadata(pool.token_account_ids[1].clone(), block_height),
                        get_rates(block_height),
                    );
                    println!("RATES: {:?}", rates);
                    let rates = rates.ok()?;
                    let rates = vec![
                        rates
                            .get(&pool.token_account_ids[0])
                            .copied()
                            .unwrap_or(precision.to_u128()?),
                        rates
                            .get(&pool.token_account_ids[1])
                            .copied()
                            .unwrap_or(precision.to_u128()?),
                    ];
                    println!("RATES (): {:?}", rates);
                    let decimals = [
                        token0_metadata.ok()?.decimals,
                        token1_metadata.ok()?.decimals,
                    ];
                    let current_c_amounts_rated = pool
                        .c_amounts
                        .iter()
                        .copied()
                        .zip(rates.iter().copied())
                        .map(|(c_amount, rate)| mul_rated(c_amount, rate))
                        .collect::<Option<Vec<_>>>()?;

                    println!("c_amounts: {:?}", pool.c_amounts);
                    println!(
                        "current_c_amounts_rated: {:?}",
                        current_c_amounts_rated
                            .iter()
                            .map(|x| format!("{x}"))
                            .collect::<Vec<_>>()
                    );

                    let ratios = [(1, 0), (0, 1)]
                        .iter()
                        .map(|&(token_in_idx, token_out_idx)| {
                            let rate_in = rates[token_in_idx];
                            let rate_out = rates[token_out_idx];

                            let token_in_amount = 1;
                            let token_in_amount_rated = mul_rated(token_in_amount, rate_in)?;

                            let new_x_amount =
                                &token_in_amount_rated + &current_c_amounts_rated[token_in_idx];

                            let y = stableswap_compute_y(
                                new_x_amount,
                                &current_c_amounts_rated,
                                token_in_idx,
                                token_out_idx,
                                stableswap_compute_amp_factor(
                                    pool.init_amp_factor,
                                    pool.target_amp_factor,
                                    pool.init_amp_time as u128,
                                    pool.stop_amp_time as u128,
                                )?,
                            )?;

                            let current_y_rated = &current_c_amounts_rated[token_out_idx];
                            let dy = if y > *current_y_rated {
                                BigDecimal::zero()
                            } else {
                                current_y_rated - &y
                            };
                            let amount_swapped = div_rated(&dy, rate_out)?;

                            println!("amount in (rated): {}", token_in_amount_rated);
                            println!("amount out (rated): {}", dy);
                            println!("y: {}", y);
                            println!("dy: {}", dy);
                            println!("amount_swapped: {}", amount_swapped);
                            println!(
                                "RATEDSWAP: 1 x {} = {} {}",
                                pool.token_account_ids[token_in_idx],
                                amount_swapped,
                                pool.token_account_ids[token_out_idx]
                            );

                            Some(
                                amount_swapped
                                    / BigDecimal::from_u128(10u128.pow(decimals[token_in_idx]))?
                                    * BigDecimal::from_u128(10u128.pow(decimals[token_out_idx]))?,
                            )
                        })
                        .collect::<Option<Vec<_>>>()?;

                    Some(PoolData {
                        tokens: (
                            pool.token_account_ids[0].clone(),
                            pool.token_account_ids[1].clone(),
                        ),
                        ratios: (ratios[0].clone(), ratios[1].clone()),
                        liquidity: (pool.c_amounts[0].into(), pool.c_amounts[1].into()),
                    })
                }
                RefPool::DegenSwapPool(pool) => {
                    println!("DEGENSWAP: {:?}", pool.token_account_ids);
                    if pool.token_account_ids.len() != 2 {
                        return None;
                    }

                    let precision = BigDecimal::from_u128(10u128.pow(24))?;

                    let mul_degen = |amount: Balance, degen: Balance| -> Option<BigDecimal> {
                        let amount_bd = BigDecimal::from_u128(amount)?;
                        let degen_bd = BigDecimal::from_u128(degen)?;
                        Some(amount_bd * degen_bd / &precision)
                    };

                    let div_degen = |amount: &BigDecimal, degen: Balance| -> Option<BigDecimal> {
                        let degen_bd = BigDecimal::from_u128(degen)?;
                        Some(amount * &precision / degen_bd)
                    };

                    let (token0_metadata, token1_metadata, degens) = tokio::join!(
                        get_token_metadata(pool.token_account_ids[0].clone(), block_height),
                        get_token_metadata(pool.token_account_ids[1].clone(), block_height),
                        get_degens(block_height),
                    );
                    println!("DEGENS: {:?}", degens);
                    let degens = degens.ok()?;
                    let degens = [
                        degens.get(&pool.token_account_ids[0]).copied()?,
                        degens.get(&pool.token_account_ids[1]).copied()?,
                    ];
                    let decimals = [
                        token0_metadata.ok()?.decimals,
                        token1_metadata.ok()?.decimals,
                    ];
                    let current_c_amounts_degen = pool
                        .c_amounts
                        .iter()
                        .copied()
                        .zip(degens.iter().copied())
                        .map(|(c_amount, degen)| mul_degen(c_amount, degen))
                        .collect::<Option<Vec<_>>>()?;

                    println!("c_amounts: {:?}", pool.c_amounts);
                    println!(
                        "current_c_amounts_degen: {:?}",
                        current_c_amounts_degen
                            .iter()
                            .map(|x| format!("{x}"))
                            .collect::<Vec<_>>()
                    );

                    let ratios = [(1, 0), (0, 1)]
                        .iter()
                        .map(|&(token_in_idx, token_out_idx)| {
                            let degen_in = degens[token_in_idx];
                            let degen_out = degens[token_out_idx];

                            let token_in_amount = 1;
                            let token_in_amount_degen = mul_degen(token_in_amount, degen_in)?;

                            let new_x_amount =
                                &token_in_amount_degen + &current_c_amounts_degen[token_in_idx];

                            let y = stableswap_compute_y(
                                new_x_amount,
                                &current_c_amounts_degen,
                                token_in_idx,
                                token_out_idx,
                                stableswap_compute_amp_factor(
                                    pool.init_amp_factor,
                                    pool.target_amp_factor,
                                    pool.init_amp_time as u128,
                                    pool.stop_amp_time as u128,
                                )?,
                            )?;

                            let current_y_degen = &current_c_amounts_degen[token_out_idx];
                            let dy = if y > *current_y_degen {
                                BigDecimal::zero()
                            } else {
                                current_y_degen - &y
                            };
                            let amount_swapped = div_degen(&dy, degen_out)?;

                            println!("y: {}", y);
                            println!("dy: {}", dy);
                            println!("amount_swapped: {}", amount_swapped);
                            println!(
                                "DEGENSWAP: 1 x {} = {} {}",
                                pool.token_account_ids[token_in_idx],
                                amount_swapped,
                                pool.token_account_ids[token_out_idx]
                            );

                            Some(
                                amount_swapped
                                    / BigDecimal::from_u128(10u128.pow(decimals[token_in_idx]))?
                                    * BigDecimal::from_u128(10u128.pow(decimals[token_out_idx]))?,
                            )
                        })
                        .collect::<Option<Vec<_>>>()?;

                    Some(PoolData {
                        tokens: (
                            pool.token_account_ids[0].clone(),
                            pool.token_account_ids[1].clone(),
                        ),
                        ratios: (ratios[0].clone(), ratios[1].clone()),
                        liquidity: (pool.c_amounts[0].into(), pool.c_amounts[1].into()),
                    })
                }
            }
        }
        PoolType::Aidols(pool) => {
            if pool.is_deployed {
                return Some(PoolData {
                    tokens: (
                        if is_testnet() {
                            "wrap.testnet".parse().unwrap()
                        } else {
                            "wrap.near".parse().unwrap()
                        },
                        pool.token_id.clone(),
                    ),
                    ratios: (0.into(), 0.into()),
                    liquidity: (0.into(), 0.into()),
                });
            }

            let amount0 = BigDecimal::from_str(&pool.wnear_hold.to_string()).ok()?;
            let amount1 = BigDecimal::from_str(&pool.token_hold.to_string()).ok()?;

            if amount0 == 0.into() || amount1 == 0.into() {
                return None;
            }
            let token0_in_1_token1 = amount0.clone() / amount1.clone();
            let token1_in_1_token0 = amount1.clone() / amount0.clone();

            Some(PoolData {
                tokens: (
                    if is_testnet() {
                        "wrap.testnet".parse().unwrap()
                    } else {
                        "wrap.near".parse().unwrap()
                    },
                    pool.token_id.clone(),
                ),
                ratios: (token0_in_1_token1, token1_in_1_token0),
                liquidity: (amount0, amount1),
            })
        }
    }
}
