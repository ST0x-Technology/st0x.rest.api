#[macro_use]
extern crate rocket;

mod error;
mod routes;
mod types;

use rain_orderbook_js_api::registry::DotrainRegistry;
use rocket_cors::{AllowedHeaders, AllowedMethods, AllowedOrigins, CorsOptions};
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
        .map(|s| std::str::FromStr::from_str(s).unwrap())
        .collect();

    CorsOptions {
        allowed_origins: AllowedOrigins::all(),
        allowed_methods,
        allowed_headers: AllowedHeaders::all(),
        allow_credentials: false,
        ..Default::default()
    }
}

async fn rocket() -> rocket::Rocket<rocket::Build> {
    dotenvy::dotenv().ok();

    let registry_url = std::env::var("REGISTRY_URL").unwrap_or_else(|_| {
        eprintln!("REGISTRY_URL environment variable must be set");
        std::process::exit(1);
    });

    let registry = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|_| std::process::exit(1));
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            DotrainRegistry::new(registry_url)
                .await
                .unwrap_or_else(|e| {
                    eprintln!("Failed to load registry: {e}");
                    std::process::exit(1);
                })
        }))
    })
    .await
    .unwrap_or_else(|_| std::process::exit(1));

    let cors = configure_cors()
        .to_cors()
        .expect("CORS configuration failed");

    rocket::build()
        .manage(registry)
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
        .attach(cors)
}

#[launch]
async fn launch() -> _ {
    rocket().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::http::Status;
    use rocket::local::blocking::Client;

    fn client() -> Client {
        let cors = configure_cors()
            .to_cors()
            .expect("CORS configuration failed");

        let rocket = rocket::build()
            .mount("/", routes::health::routes())
            .attach(cors);

        Client::tracked(rocket).expect("valid rocket instance")
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
