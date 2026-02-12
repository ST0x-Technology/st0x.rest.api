#[macro_use]
extern crate rocket;

mod catchers;
mod db;
mod error;
mod fairings;
mod routes;
mod telemetry;
mod types;

use rocket_cors::{AllowedHeaders, AllowedMethods, AllowedOrigins, CorsOptions};
use std::collections::HashSet;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

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
        expose_headers: HashSet::from(["X-Request-Id".to_string()]),
        ..Default::default()
    }
    .to_cors()
    .map_err(|e| format!("CORS configuration failed: {e}"))
}

fn rocket(pool: db::DbPool) -> Result<rocket::Rocket<rocket::Build>, String> {
    let cors = configure_cors()?;

    let figment = rocket::Config::figment().merge((rocket::Config::LOG_LEVEL, "normal"));

    Ok(rocket::custom(figment)
        .manage(pool)
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
        .attach(cors))
}

#[rocket::main]
async fn main() {
    let log_guard = telemetry::init();

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

    let rocket = match rocket(pool) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::http::Status;
    use rocket::local::asynchronous::Client;

    async fn client() -> Client {
        let id = uuid::Uuid::new_v4();
        let pool = db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("database init");
        Client::tracked(rocket(pool).expect("valid rocket instance"))
            .await
            .expect("valid client")
    }

    #[rocket::async_test]
    async fn test_health_endpoint() {
        let client = client().await;
        let response = client.get("/health").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
    }
}
