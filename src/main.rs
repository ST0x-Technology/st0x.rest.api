#[macro_use]
extern crate rocket;

mod auth;
mod catchers;
mod cli;
mod db;
mod error;
mod fairings;
mod raindex;
mod routes;
mod telemetry;
mod types;

#[cfg(test)]
mod test_helpers;

use clap::Parser;
use rocket_cors::{AllowedHeaders, AllowedMethods, AllowedOrigins, CorsOptions};
use std::collections::HashSet;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_swagger_ui::SwaggerUi;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            let mut scheme = Http::new(HttpAuthScheme::Basic);
            scheme.description = Some(
                "Use your API key as the username and API secret as the password.".to_string(),
            );
            components.add_security_scheme("basicAuth", SecurityScheme::Http(scheme));
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        routes::health::get_health,
        routes::tokens::get_tokens,
        routes::swap::post_swap_quote,
        routes::swap::post_swap_calldata,
        routes::order::post_order_dca,
        routes::order::post_order_solver,
        routes::order::get_order,
        routes::order::post_order_cancel,
        routes::orders::get_orders_by_tx,
        routes::orders::get_orders_by_address,
        routes::trades::get_trades_by_tx,
        routes::trades::get_trades_by_address,
    ),
    components(),
    modifiers(&SecurityAddon),
    tags(
        (name = "Health", description = "Health check endpoints"),
        (name = "Tokens", description = "Token information endpoints"),
        (name = "Swap", description = "Swap quote and calldata endpoints"),
        (name = "Order", description = "Order deployment and management endpoints"),
        (name = "Orders", description = "Order listing and query endpoints"),
        (name = "Trades", description = "Trade listing and query endpoints"),
    ),
    info(
        title = "st0x REST API",
        version = "0.1.0",
        description = "REST API for st0x orderbook operations",
    )
)]
struct ApiDoc;

fn configure_cors() -> Result<rocket_cors::Cors, String> {
    let allowed_methods: AllowedMethods = ["Get", "Post", "Options"]
        .iter()
        .map(|s| {
            std::str::FromStr::from_str(s)
                .map_err(|_| format!("invalid HTTP method in CORS config: {s}"))
        })
        .collect::<Result<_, _>>()?;

    CorsOptions {
        allowed_origins: AllowedOrigins::all(),
        allowed_methods,
        allowed_headers: AllowedHeaders::all(),
        allow_credentials: false,
        expose_headers: HashSet::from([
            "X-Request-Id".to_string(),
            "Retry-After".to_string(),
            "X-RateLimit-Limit".to_string(),
            "X-RateLimit-Remaining".to_string(),
            "X-RateLimit-Reset".to_string(),
        ]),
        ..Default::default()
    }
    .to_cors()
    .map_err(|e| format!("CORS configuration failed: {e}"))
}

pub(crate) fn rocket(
    pool: db::DbPool,
    rate_limiter: fairings::RateLimiter,
    raindex_config: raindex::RaindexProvider,
) -> Result<rocket::Rocket<rocket::Build>, String> {
    let cors = configure_cors()?;

    let figment = rocket::Config::figment().merge((rocket::Config::LOG_LEVEL, "normal"));

    Ok(rocket::custom(figment)
        .manage(pool)
        .manage(rate_limiter)
        .manage(raindex_config)
        .mount("/", routes::health::routes())
        .mount("/v1/tokens", routes::tokens::routes())
        .mount("/v1/swap", routes::swap::routes())
        .mount("/v1/order", routes::order::routes())
        .mount("/v1/orders", routes::orders::routes())
        .mount("/v1/trades", routes::trades::routes())
        .mount(
            "/",
            SwaggerUi::new("/swagger/<tail..>").url("/api-doc/openapi.json", ApiDoc::openapi()),
        )
        .register("/", catchers::catchers())
        .attach(fairings::RequestLogger)
        .attach(fairings::UsageLogger)
        .attach(fairings::RateLimitHeadersFairing)
        .attach(routes::tokens::fairing())
        .attach(cors))
}

#[rocket::main]
async fn main() {
    let parsed = cli::Cli::parse();

    let command = match parsed.command {
        Some(cmd) => cmd,
        None => {
            cli::print_usage();
            return;
        }
    };

    let log_guard = match telemetry::init() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to initialize telemetry: {e}");
            std::process::exit(1);
        }
    };

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        tracing::warn!("DATABASE_URL not set, using default: sqlite:./data/st0x.db");
        "sqlite:./data/st0x.db".to_string()
    });

    let pool = match db::init(&database_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to initialize database");
            drop(log_guard);
            std::process::exit(1);
        }
    };

    let global_rpm: u64 = std::env::var("RATE_LIMIT_GLOBAL_RPM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let per_key_rpm: u64 = std::env::var("RATE_LIMIT_PER_KEY_RPM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    tracing::info!(global_rpm, per_key_rpm, "rate limiter configured");

    match command {
        cli::Command::Serve => {
            let registry_url = match std::env::var("REGISTRY_URL") {
                Ok(url) if !url.is_empty() => url,
                _ => {
                    tracing::error!("REGISTRY_URL environment variable is required");
                    drop(log_guard);
                    std::process::exit(1);
                }
            };

            let raindex_config = match raindex::RaindexProvider::load(&registry_url).await {
                Ok(config) => {
                    tracing::info!(registry_url = %registry_url, "raindex registry loaded");
                    config
                }
                Err(e) => {
                    tracing::error!(error = %e, registry_url = %registry_url, "failed to load raindex registry");
                    drop(log_guard);
                    std::process::exit(1);
                }
            };

            let rate_limiter = fairings::RateLimiter::new(global_rpm, per_key_rpm);
            let rocket = match rocket(pool, rate_limiter, raindex_config) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build Rocket instance");
                    drop(log_guard);
                    std::process::exit(1);
                }
            };

            if let Err(e) = rocket.launch().await {
                tracing::error!(error = %e, "Rocket launch failed");
                drop(log_guard);
                std::process::exit(1);
            }
        }
        cli::Command::Keys { command } => {
            if let Err(e) = cli::handle_keys_command(command, pool).await {
                tracing::error!(error = %e, "keys command failed");
                drop(log_guard);
                std::process::exit(1);
            }
        }
    }

    drop(log_guard);
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{basic_auth_header, client, seed_api_key};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn test_health_endpoint() {
        let client = client().await;
        let response = client.get("/health").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[rocket::async_test]
    async fn test_protected_route_returns_401_without_auth() {
        let client = client().await;
        let response = client.get("/v1/tokens").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_protected_route_returns_401_with_wrong_secret() {
        let client = client().await;
        let (key_id, _) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, "wrong-secret");
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_protected_route_succeeds_with_valid_auth() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_inactive_key_returns_401() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        sqlx::query("UPDATE api_keys SET active = 0 WHERE key_id = ?")
            .bind(&key_id)
            .execute(pool)
            .await
            .expect("deactivate key");

        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
