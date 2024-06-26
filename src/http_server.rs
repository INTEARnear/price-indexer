use std::{fs::File, io::BufReader, sync::Arc};

use actix_cors::Cors;
use actix_web::{http::StatusCode, web, App, HttpResponse, HttpResponseBuilder, HttpServer, Route};
use inindexer::near_indexer_primitives::types::AccountId;
use tokio::sync::RwLock;

use crate::{token::Token, tokens::Tokens, JsonSerializedPrices, TokenIdWrapper};

pub async fn launch_http_server(
    tokens: Arc<RwLock<Tokens>>,
    json_serialized_all_tokens: Arc<RwLock<Option<JsonSerializedPrices>>>,
) {
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
        let json_serialized_all_tokens = Arc::clone(&json_serialized_all_tokens);
        App::new()
            .wrap(actix_web::middleware::Logger::new(r#"%a "%r" (forwarded from %{r}a) %s %b Referrer: "%{Referer}i" User-Agent: "%{User-Agent}i" %T"#))
            .wrap(Cors::default().allow_any_origin().allow_any_method().allow_any_header())
            .route(
                "/list-token-price",
                cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                    &json_serialized.ref_compatibility_format
                }),
            )
            .route(
                "/prices",
                cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                    &json_serialized.prices_only
                }),
            )
            .route(
                "/super-precise",
                cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                    &json_serialized.super_precise
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
                        cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                            &json_serialized.ref_compatibility_format_with_hardcoded
                        }),
                    )
                    .route(
                        "/prices",
                        cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                            &json_serialized.prices_only_with_hardcoded
                        }),
                    )
                    .route(
                        "/super-precise",
                        cached_all_tokens_route(Arc::clone(&json_serialized_all_tokens), |json_serialized| {
                            &json_serialized.super_precise_with_hardcoded
                        }),
                    )
                    .route("/get-token-price", price_route(Arc::clone(&tokens), |token_id, token| {
                        HttpResponse::Ok().content_type("text/html; charset=utf8") // why does ref send this as html
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "{}"}}"#, token.price_usd_hardcoded.with_scale(12)))
                    }, Some(|token_id| {
                        HttpResponse::Ok().content_type("text/html; charset=utf8")
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .body(format!(r#"{{"token_contract_id": "{token_id}", "price": "N/A"}}"#))
                    })))
                    .route("/price", price_route(Arc::clone(&tokens), |_, token| {
                        HttpResponse::Ok()
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .json(token.price_usd_hardcoded.to_string().parse::<f64>().unwrap())
                    }, None))
                    .route("/super-precise-price", price_route(Arc::clone(&tokens), |_, token| {
                        HttpResponse::Ok()
                            .insert_header(("Cache-Control", "public, max-age=1"))
                            .json(token.price_usd_hardcoded.to_string())
                    }, None)),
                )
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

    server
        .disable_signals()
        .run()
        .await
        .expect("Failed to start HTTP server");
}

fn cached_all_tokens_route(
    json_serialized: Arc<RwLock<Option<JsonSerializedPrices>>>,
    get_field: fn(&JsonSerializedPrices) -> &String,
) -> Route {
    web::get().to(move || {
        let json_serialized = Arc::clone(&json_serialized);
        async move {
            if let Some(json_serialized) = json_serialized.read().await.as_ref() {
                HttpResponseBuilder::new(StatusCode::OK)
                    .content_type("application/json")
                    .insert_header(("Cache-Control", "public, max-age=3"))
                    .body(get_field(json_serialized).clone())
            } else {
                HttpResponse::InternalServerError().finish()
            }
        }
    })
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
