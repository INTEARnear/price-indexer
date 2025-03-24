use std::{fs::File, io::BufReader, sync::Arc};

use actix_cors::Cors;
use actix_web::{
    http::StatusCode,
    web::{self, redirect},
    App, HttpResponse, HttpServer, Route,
};
use inindexer::near_indexer_primitives::types::AccountId;
use itertools::Itertools;
use serde::{Deserialize, Deserializer};
use tokio::sync::RwLock;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    token::{Token, TokenScore},
    tokens::Tokens,
};

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
                                .json(tokens.tokens.get(&query.token_id))
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
                            HttpResponse::Ok()
                                .insert_header(("Cache-Control", "public, max-age=1"))
                                .json(results)
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
