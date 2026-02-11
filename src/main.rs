#[macro_use]
extern crate rocket;

mod catchers;
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

fn configure_cors() -> CorsOptions {
    let allowed_methods: AllowedMethods = ["Get", "Post", "Options"]
        .iter()
        .map(|s| {
            std::str::FromStr::from_str(s).unwrap_or_else(|_| {
                tracing::error!(method = %s, "invalid HTTP method in CORS config");
                std::process::exit(1);
            })
        })
        .collect();

    CorsOptions {
        allowed_origins: AllowedOrigins::all(),
        allowed_methods,
        allowed_headers: AllowedHeaders::all(),
        allow_credentials: false,
        expose_headers: HashSet::from(["X-Request-Id".to_string()]),
        ..Default::default()
    }
}

fn rocket() -> rocket::Rocket<rocket::Build> {
    let cors = match configure_cors().to_cors() {
        Ok(cors) => cors,
        Err(e) => {
            tracing::error!(error = %e, "CORS configuration failed");
            std::process::exit(1);
        }
    };

    let config = rocket::Config {
        log_level: rocket::config::LogLevel::Normal,
        ..rocket::Config::default()
    };

    rocket::custom(config)
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
        .attach(cors)
}

#[rocket::main]
async fn main() {
    let _log_guard = telemetry::init();
    if let Err(e) = rocket().launch().await {
        tracing::error!(error = %e, "Rocket launch failed");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::http::Status;
    use rocket::local::blocking::Client;

    fn client() -> Client {
        Client::tracked(rocket()).expect("valid rocket instance")
    }

    #[test]
    fn test_health_endpoint() {
        let client = client();
        let response = client.get("/health").dispatch();
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
    }
}
