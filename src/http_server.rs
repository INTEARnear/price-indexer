use std::{collections::HashMap, fs::File, io::BufReader, sync::Arc};

use actix_cors::Cors;
use actix_web::{
    http::StatusCode,
    web::{self, redirect},
    App, HttpResponse, HttpServer, Route,
};
use base64::{prelude::BASE64_STANDARD, Engine};
use inindexer::{
    near_indexer_primitives::{
        types::{AccountId, BlockReference, Finality},
        views::QueryRequest,
    },
    near_utils::FtBalance,
};
use itertools::Itertools;
use near_jsonrpc_client::{
    methods::query::{RpcQueryRequest, RpcQueryResponse},
    JsonRpcClient,
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::RwLock;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    network::is_testnet,
    token::{Token, TokenScore},
    tokens::Tokens,
    utils::{get_rpc_url, get_user_token_balances},
};

const WRAP_NEAR_ICON: &str = "data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iMTA4MCIgaGVpZ2h0PSIxMDgwIiB2aWV3Qm94PSIwIDAgMTA4MCAxMDgwIiBmaWxsPSJub25lIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciPgo8cmVjdCB3aWR0aD0iMTA4MCIgaGVpZ2h0PSIxMDgwIiBmaWxsPSIjMDBFQzk3Ii8+CjxwYXRoIGQ9Ik03NzMuNDI1IDI0My4zOEM3NTEuNDUzIDI0My4zOCA3MzEuMDU0IDI1NC43NzIgNzE5LjU0NCAyNzMuNDk5TDU5NS41MzggNDU3LjYwNkM1OTEuNDk5IDQ2My42NzMgNTkzLjEzOCA0NzEuODU0IDU5OS4yMDYgNDc1Ljg5M0M2MDQuMTI0IDQ3OS4xNzIgNjEwLjYzMSA0NzguNzY2IDYxNS4xMSA0NzQuOTEzTDczNy4xNzIgMzY5LjA0MkM3MzkuMiAzNjcuMjE3IDc0Mi4zMjcgMzY3LjQwMyA3NDQuMTUyIDM2OS40MzFDNzQ0Ljk4IDM3MC4zNjEgNzQ1LjQyIDM3MS41NjEgNzQ1LjQyIDM3Mi43OTRWNzA0LjI2NUM3NDUuNDIgNzA3LjAwMyA3NDMuMjA2IDcwOS4yIDc0MC40NjggNzA5LjJDNzM4Ljk5NyA3MDkuMiA3MzcuNjExIDcwOC41NTggNzM2LjY4MiA3MDcuNDI1TDM2Ny43MDcgMjY1Ljc1OEMzNTUuNjkgMjUxLjU3NyAzMzguMDQ1IDI0My4zOTcgMzE5LjQ3IDI0My4zOEgzMDYuNTc1QzI3MS42NzMgMjQzLjM4IDI0My4zOCAyNzEuNjczIDI0My4zOCAzMDYuNTc1Vjc3My40MjVDMjQzLjM4IDgwOC4zMjcgMjcxLjY3MyA4MzYuNjIgMzA2LjU3NSA4MzYuNjJDMzI4LjU0NiA4MzYuNjIgMzQ4Ljk0NiA4MjUuMjI4IDM2MC40NTYgODA2LjUwMUw0ODQuNDYyIDYyMi4zOTRDNDg4LjUwMSA2MTYuMzI3IDQ4Ni44NjIgNjA4LjE0NiA0ODAuNzk0IDYwNC4xMDdDNDc1Ljg3NiA2MDAuODI4IDQ2OS4zNjkgNjAxLjIzNCA0NjQuODkgNjA1LjA4N0wzNDIuODI4IDcxMC45NThDMzQwLjggNzEyLjc4MyAzMzcuNjczIDcxMi41OTcgMzM1Ljg0OCA3MTAuNTY5QzMzNS4wMiA3MDkuNjM5IDMzNC41OCA3MDguNDM5IDMzNC41OTcgNzA3LjIwNlYzNzUuNjUxQzMzNC41OTcgMzcyLjkxMyAzMzYuODExIDM3MC43MTUgMzM5LjU0OSAzNzAuNzE1QzM0MS4wMDMgMzcwLjcxNSAzNDIuNDA2IDM3MS4zNTggMzQzLjMzNSAzNzIuNDlMNzEyLjI1OSA4MTQuMjQyQzcyNC4yNzYgODI4LjQyMyA3NDEuOTIxIDgzNi42MDMgNzYwLjQ5NiA4MzYuNjJINzczLjM5MkM4MDguMjkzIDgzNi42MzcgODM2LjYwMyA4MDguMzYxIDgzNi42MzcgNzczLjQ1OVYzMDYuNTc1QzgzNi42MzcgMjcxLjY3MyA4MDguMzQ0IDI0My4zOCA3NzMuNDQyIDI0My4zOEg3NzMuNDI1WiIgZmlsbD0iYmxhY2siLz4KPC9zdmc+";

pub async fn launch_http_server(tokens: Arc<RwLock<Tokens>>) {
    let tls_config = if let Ok(files) = std::env::var("SSL") {
        #[allow(clippy::iter_nth_zero)]
        let mut certs_file = BufReader::new(File::open(files.split(',').nth(0).unwrap()).unwrap());
        let mut key_file = BufReader::new(File::open(files.split(',').nth(1).unwrap()).unwrap());
        let tls_certs = rustls_pemfile::certs(&mut certs_file)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let tls_key = rustls_pemfile::pkcs8_private_keys(&mut key_file)
            .next()
            .unwrap()
            .unwrap();
        Some(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(tls_certs, rustls::pki_types::PrivateKeyDer::Pkcs8(tls_key))
                .unwrap(),
        )
    } else {
        None
    };

    let server = HttpServer::new(move || {
        let tokens = Arc::clone(&tokens);

        App::new()
            .route("/openapi", web::get().to(|| async { HttpResponse::with_body(StatusCode::OK, include_str!("../openapi.yml")) }))
            .service(SwaggerUi::new("/swagger-ui/{_:.*}").config(utoipa_swagger_ui::Config::from("/openapi")))
            .service(redirect("/", "/swagger-ui/"))
            .wrap(actix_web::middleware::Logger::new(r#"%a "%r" (forwarded from %{r}a) %s %b Referrer: "%{Referer}i" User-Agent: "%{User-Agent}i" %T"#))
            .wrap(Cors::default().allow_any_origin().allow_any_method().allow_any_header())
            .route(
                "/list-token-price",
                web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            let ref_compatibility_format: serde_json::Value = tokens.tokens.iter()
                                .filter(|(_, token)| !token.deleted)
                                .map(|(token_id, token)| (token_id.clone(), serde_json::json!({
                                    "price": token.price_usd.with_scale(12).to_string(),
                                    "symbol": token.metadata.symbol,
                                    "decimal": token.metadata.decimals,
                                })))
                                .sorted_by_key(|(token_id, _)| token_id.to_string())
                                .collect();
                            HttpResponse::Ok()
                                .content_type("application/json")
                                .insert_header(("Cache-Control", "public, max-age=5"))
                                .json(ref_compatibility_format)
                        }
                    }
                }),
            )
            .route(
                "/prices",
                web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            let prices: serde_json::Value = tokens.tokens.iter()
                                .filter(|(_, token)| !token.deleted)
                                .map(|(token_id, token)| (token_id.clone(), token.price_usd.to_string().parse::<f64>().unwrap()))
                                .collect();
                            HttpResponse::Ok()
                                .content_type("application/json")
                                .insert_header(("Cache-Control", "public, max-age=5"))
                                .json(prices)
                        }
                    }
                }),
            )
            .route(
                "/super-precise",
                web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            let prices: serde_json::Value = tokens.tokens.iter()
                                .filter(|(_, token)| !token.deleted)
                                .map(|(token_id, token)| (token_id.clone(), token.price_usd.to_string()))
                                .collect();
                            HttpResponse::Ok()
                                .content_type("application/json")
                                .insert_header(("Cache-Control", "public, max-age=5"))
                                .json(prices)
                        }
                    }
                }),
            )
            .route("/get-token-price", price_route(Arc::clone(&tokens), |token_id, token| {
                HttpResponse::Ok().content_type("text/html; charset=utf8") // why does ref send this as html
                    .insert_header(("Cache-Control", "public, max-age=1"))
                    .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "{}"}}"#, token.price_usd.with_scale(12)))
            }, Some(|token_id| {
                HttpResponse::Ok().content_type("text/html; charset=utf8")
                    .insert_header(("Cache-Control", "public, max-age=1"))
                    .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "N/A"}}"#))
            })))
            .route("/price", price_route(Arc::clone(&tokens), |_, token| {
                HttpResponse::Ok()
                    .insert_header(("Cache-Control", "public, max-age=1"))
                    .json(token.price_usd.to_string().parse::<f64>().unwrap())
            }, None))
            .route("/super-precise-price", price_route(Arc::clone(&tokens), |_, token| {
                HttpResponse::Ok()
                    .insert_header(("Cache-Control", "public, max-age=1"))
                    .json(token.price_usd.to_string())
            }, None))
            .service(
                web::scope("/hardcoded")
                    .route(
                        "/list-token-price",
                        web::get().to({
                            let tokens = Arc::clone(&tokens);
                            move || {
                                let tokens = Arc::clone(&tokens);
                                async move {
                                    let tokens = tokens.read().await;
                                    let ref_compatibility_format: serde_json::Value = tokens.tokens.iter()
                                        .filter(|(_, token)| !token.deleted)
                                        .map(|(token_id, token)| (token_id.clone(), serde_json::json!({
                                            "price": token.price_usd_hardcoded.with_scale(12).to_string(),
                                            "symbol": token.metadata.symbol,
                                            "decimal": token.metadata.decimals,
                                        })))
                                        .sorted_by_key(|(token_id, _)| token_id.to_string())
                                        .collect();
                                    HttpResponse::Ok()
                                        .content_type("application/json")
                                        .insert_header(("Cache-Control", "public, max-age=5"))
                                        .json(ref_compatibility_format)
                                }
                            }
                        }),
                    )
                    .route(
                        "/prices",
                        web::get().to({
                            let tokens = Arc::clone(&tokens);
                            move || {
                                let tokens = Arc::clone(&tokens);
                                async move {
                                    let tokens = tokens.read().await;
                                    let prices: serde_json::Value = tokens.tokens.iter()
                                        .filter(|(_, token)| !token.deleted)
                                        .map(|(token_id, token)| (token_id.clone(), token.price_usd_hardcoded.to_string().parse::<f64>().unwrap()))
                                        .collect();
                                    HttpResponse::Ok()
                                        .content_type("application/json")
                                        .insert_header(("Cache-Control", "public, max-age=5"))
                                        .json(prices)
                                }
                            }
                        }),
                    )
                    .route(
                        "/super-precise",
                        web::get().to({
                            let tokens = Arc::clone(&tokens);
                            move || {
                                let tokens = Arc::clone(&tokens);
                                async move {
                                    let tokens = tokens.read().await;
                                    let prices: serde_json::Value = tokens.tokens.iter()
                                        .filter(|(_, token)| !token.deleted)
                                        .map(|(token_id, token)| (token_id.clone(), token.price_usd_hardcoded.to_string()))
                                        .collect();
                                    HttpResponse::Ok()
                                        .content_type("application/json")
                                        .insert_header(("Cache-Control", "public, max-age=5"))
                                        .json(prices)
                                }
                            }
                        }),
                    )
                    .route("/get-token-price", price_route(Arc::clone(&tokens), |token_id, token| {
                        HttpResponse::Ok().content_type("text/html; charset=utf8") // why does ref send this as html
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "{}"}}"#, token.price_usd.with_scale(12)))
                    }, Some(|token_id| {
                        HttpResponse::Ok().content_type("text/html; charset=utf8")
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "N/A"}}"#))
                    })))
                    .route("/price", price_route(Arc::clone(&tokens), |_, token| {
                        HttpResponse::Ok()
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .json(token.price_usd.to_string().parse::<f64>().unwrap())
                    }, None))
                    .route("/super-precise-price", price_route(Arc::clone(&tokens), |_, token| {
                        HttpResponse::Ok()
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .json(token.price_usd.to_string())
                    }, None)),
                )
                .route("/token", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move |query: web::Query<TokenIdWrapper>| {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(match tokens.tokens.get(&query.token_id) {
                                    Some(token) => serialize_with_icon(token),
                                    None => {
                                        return HttpResponse::NotFound().finish();
                                    }
                                })
                        }
                    }
                }))
                .route("/tokens", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .content_type("application/json")
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(&tokens.tokens)
                        }
                    }
                }))
                .route("/tokens-advanced", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move |query: web::Query<TokensCriteria>| {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens.tokens.values().filter(|token| {
                                    token.reputation >= query.min_reputation
                                    && (query.account_ids.is_empty() || query.account_ids.contains(&token.account_id))
                                    && query.platform.as_ref().is_none_or(|platform| token.account_id.as_str().ends_with(&format!(".{platform}")))
                                }).take(query.take).collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/token-search", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move |query: web::Query<TokenSearch>| {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            let results = tokens.search_tokens(
                                &query.query.to_lowercase(),
                                query.take,
                                query.reputation,
                                query.account_id.clone(),
                                query.platform.clone(),
                            ).await;
                            let results_with_icons: Vec<serde_json::Value> = results
                                .into_iter()
                                .map(serialize_with_icon)
                                .collect();
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(results_with_icons)
                        }
                    }
                }))
                .route("/token-spam-list", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(&tokens.spam_tokens)
                        }
                    }
                }))
                .route("/token-unknown-or-better-list", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::Unknown)
                                    .map(|token| &token.account_id)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/tokens-unknown-or-better", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::Unknown)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/token-notfake-or-better-list", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::NotFake)
                                    .map(|token| &token.account_id)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/tokens-notfake-or-better", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::NotFake)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/reputable-list", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::Reputable)
                                    .map(|token| &token.account_id)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/tokens-reputable", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move || {
                        let tokens = Arc::clone(&tokens);
                        async move {
                            let tokens = tokens.read().await;
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(tokens
                                    .tokens
                                    .values()
                                    .filter(|token| token.reputation >= TokenScore::Reputable)
                                    .collect::<Vec<_>>())
                        }
                    }
                }))
                .route("/get-user-tokens", web::get().to({
                    let tokens = Arc::clone(&tokens);
                    move |query: web::Query<GetUserTokensRequest>| {
                        let tokens = Arc::clone(&tokens);

                        async move {
                            #[derive(Debug)]
                            struct TokenBalanceResponse {
                                token: AccountId,
                                balance: FtBalance,
                                source: TokenBalanceSource,
                            }

                            let account_id = query.account_id.clone();
                            let direct = async move {
                                match get_user_token_balances(account_id.clone()).await {
                                    Ok(balances) => {
                                        let wrap = if is_testnet() {
                                            "wrap.testnet".parse().unwrap()
                                        } else {
                                            "wrap.near".parse().unwrap()
                                        };
                                        let has_wnear = balances.tokens.iter().any(|balance| balance.contract_id == wrap);
                                        let mut balances = balances.tokens.into_iter().map(|balance| TokenBalanceResponse {
                                            token: balance.contract_id,
                                            balance: if balance.balance.is_empty() {
                                                0
                                            } else {
                                                balance.balance.parse::<u128>().unwrap_or_default()
                                            },
                                            source: TokenBalanceSource::Direct,
                                        }).collect::<Vec<_>>();
                                        if !has_wnear {
                                            balances.push(TokenBalanceResponse {
                                                token: wrap,
                                                balance: 0,
                                                source: TokenBalanceSource::Direct,
                                            });
                                        }
                                        balances
                                    }
                                    Err(_) => {
                                        vec![]
                                    }
                                }
                            };

                            let account_id = query.account_id.clone();
                            let rhea = async move {
                                let client = JsonRpcClient::connect(get_rpc_url());
                                client.call(RpcQueryRequest {
                                    block_reference: BlockReference::Finality(Finality::None),
                                    request: QueryRequest::CallFunction {
                                        account_id: "v2.ref-finance.near".parse().unwrap(),
                                        method_name: "get_deposits".into(),
                                        args: serde_json::to_vec(&serde_json::json!({
                                            "account_id": account_id
                                        })).unwrap().into(),
                                    },
                                })
                                .await
                                .map(|response| match response.kind {
                                    QueryResponseKind::CallResult(call_result) => {
                                        let map: HashMap<AccountId, String> = serde_json::from_slice(&call_result.result).unwrap();
                                        map.into_iter().map(|(token, balance)| TokenBalanceResponse {
                                            token,
                                            balance: balance.parse::<u128>().unwrap_or_default(),
                                            source: TokenBalanceSource::Rhea,
                                        }).collect::<Vec<_>>()
                                    }
                                    _ => vec![],
                                })
                                .unwrap_or_default()
                            };

                            let account_id = query.account_id.clone();
                            let native = async move {
                                let client = JsonRpcClient::connect(get_rpc_url());
                                let balance = match client.call(RpcQueryRequest {
                                    block_reference: BlockReference::Finality(Finality::None),
                                    request: QueryRequest::ViewAccount {
                                        account_id: account_id.clone(),
                                    },
                                }).await {
                                    Ok(RpcQueryResponse { kind: QueryResponseKind::ViewAccount(view_account), .. }) => {
                                        view_account.amount.as_yoctonear()
                                    }
                                    _ => 0,
                                };
                                vec![TokenBalanceResponse {
                                    token: "near".parse().unwrap(),
                                    balance,
                                    source: TokenBalanceSource::Native,
                                }]
                            };

                            let mut sources = vec![];
                            if query.direct {
                                sources.push(tokio::spawn(direct));
                            }
                            if query.rhea {
                                sources.push(tokio::spawn(rhea));
                            }
                            if query.native {
                                sources.push(tokio::spawn(native));
                            }

                            let balances = futures_util::future::join_all(sources).await;
                            let balances = balances.into_iter().flatten().flatten().collect::<Vec<_>>();

                            let tokens = tokens.read().await;
                            let response: serde_json::Value = balances.into_iter()
                                .filter_map(|balance| {
                                    if let Some(token_info) = tokens.tokens.get(&balance.token) {
                                        Some(serde_json::json!({
                                            "token": serialize_with_icon(token_info),
                                            "balance": balance.balance.to_string(),
                                            "source": balance.source,
                                        }))
                                    } else if matches!(balance.source, TokenBalanceSource::Native) {
                                        let wrap_id: AccountId = if is_testnet() {
                                            "wrap.testnet".parse().unwrap()
                                        } else {
                                            "wrap.near".parse().unwrap()
                                        };
                                        let mut native_token = tokens.tokens.get(&wrap_id).unwrap().clone();
                                        native_token.account_id = balance.token;
                                        native_token.metadata.symbol = "NEAR".to_string();
                                        native_token.metadata.name = "NEAR".to_string();
                                        native_token.main_pool = None;
                                        native_token.created_at = 0;
                                        Some(serde_json::json!({
                                            "token": serialize_with_icon(&native_token),
                                            "balance": balance.balance.to_string(),
                                            "source": balance.source,
                                        }))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=3"))
                                .json(response)
                        }
                    }
                }))
    });

    let server = if let Some(tls_config) = tls_config {
        server
            .bind_rustls_0_22(
                std::env::var("BIND_ADDRESS").unwrap_or("0.0.0.0:8080".to_string()),
                tls_config,
            )
            .expect("Failed to bind HTTP server")
    } else {
        server
            .bind(std::env::var("BIND_ADDRESS").unwrap_or("0.0.0.0:8080".to_string()))
            .expect("Failed to bind HTTP server")
    };

    server.run().await.expect("Failed to start HTTP server");
}

fn price_route(
    tokens: Arc<RwLock<Tokens>>,
    respond: fn(AccountId, &Token) -> HttpResponse,
    respond_404: Option<fn(AccountId) -> HttpResponse>,
) -> Route {
    web::get().to(move |query: web::Query<TokenIdWrapper>| {
        let token_id = query.into_inner().token_id;
        let tokens = Arc::clone(&tokens);
        async move {
            let tokens = tokens.read().await;
            if let Some(token) = tokens.tokens.get(&token_id) {
                respond(token_id, token)
            } else if let Some(respond_404) = respond_404 {
                respond_404(token_id)
            } else {
                HttpResponse::NotFound().finish()
            }
        }
    })
}

#[derive(Debug, Deserialize)]
struct TokenSearch {
    #[serde(rename = "q")]
    query: String,
    #[serde(rename = "n", default = "default_search_take")]
    take: usize,
    #[serde(rename = "rep", default)]
    reputation: TokenScore,
    #[serde(rename = "acc", default)]
    account_id: Option<AccountId>,
    #[serde(default)]
    platform: Option<AccountId>,
}

fn default_search_take() -> usize {
    5
}

#[derive(Debug, Deserialize)]
struct TokenIdWrapper {
    token_id: AccountId,
}

#[derive(Debug, Deserialize)]
struct TokensCriteria {
    #[serde(default = "usize_max")]
    take: usize,
    #[serde(default)]
    min_reputation: TokenScore,
    #[serde(default, deserialize_with = "deserialize_comma_separated_account_ids")]
    account_ids: Vec<AccountId>,
    #[serde(default)]
    platform: Option<AccountId>,
}

fn usize_max() -> usize {
    usize::MAX
}

fn deserialize_comma_separated_account_ids<'de, D>(
    deserializer: D,
) -> Result<Vec<AccountId>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(s.split(',')
        .map(|s| s.parse::<AccountId>())
        .filter_map(|result| result.ok())
        .collect())
}

fn serialize_with_icon(token: &Token) -> serde_json::Value {
    serde_json::json!({
        "account_id": token.account_id,
        "price_usd_raw": token.price_usd_raw.to_string(),
        "price_usd": token.price_usd.to_string(),
        "price_usd_hardcoded": token.price_usd_hardcoded.to_string(),
        "price_usd_raw_24h_ago": token.price_usd_raw_24h_ago.to_string(),
        "main_pool": token.main_pool,
        "metadata": {
            "name": token.metadata.name,
            "symbol": token.metadata.symbol,
            "decimals": token.metadata.decimals,
            "reference": token.metadata.reference,
            "icon": if token.account_id == "wrap.near" || token.account_id == "wrap.testnet" {
                Some(WRAP_NEAR_ICON.to_string())
            } else if token.account_id == "ztarknear-1845.meme-cooking.near" {
                Some(format!("data:image/webp;base64,{}", BASE64_STANDARD.encode(include_bytes!("../icon_overrides/ztarknear-1845.meme-cooking.near.webp"))))
            } else {
                token.metadata.icon.clone()
            },
        },
        "total_supply": token.total_supply.to_string(),
        "circulating_supply": token.circulating_supply.to_string(),
        "circulating_supply_excluding_team": token.circulating_supply_excluding_team.to_string(),
        "reputation": token.reputation,
        "socials": token.socials,
        "slug": token.slug,
        "deleted": token.deleted,
        "reference": token.reference,
        "liquidity_usd": token.liquidity_usd,
        "volume_usd_24h": token.volume_usd_24h,
        "created_at": token.created_at
    })
}

#[derive(Debug, Deserialize)]
struct GetUserTokensRequest {
    account_id: AccountId,
    #[serde(default = "default_direct")]
    direct: bool,
    #[serde(default)]
    rhea: bool,
    #[serde(default)]
    native: bool,
}

fn default_direct() -> bool {
    true
}

#[derive(Debug, Serialize)]
enum TokenBalanceSource {
    Direct,
    Rhea,
    Native,
}
