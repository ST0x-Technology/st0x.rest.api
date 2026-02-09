#[macro_use]
extern crate rocket;

mod error;
mod routes;
mod types;

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

fn rocket() -> rocket::Rocket<rocket::Build> {
    let cors = configure_cors().to_cors().expect("CORS configuration failed");

    rocket::build()
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
fn launch() -> _ {
    rocket()
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
        let body: serde_json::Value = serde_json::from_str(&response.into_string().unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
    }

    fn get_openapi_json(client: &Client) -> serde_json::Value {
        let response = client.get("/api-doc/openapi.json").dispatch();
        assert_eq!(response.status(), Status::Ok);
        let body = response.into_string().unwrap();
        serde_json::from_str(&body).unwrap()
    }

    #[test]
    fn test_openapi_json_contains_all_paths() {
        let client = client();
        let spec = get_openapi_json(&client);
        let paths = spec["paths"].as_object().unwrap();

        let expected_paths = [
            "/health",
            "/v1/tokens",
            "/v1/swap/quote",
            "/v1/swap/calldata",
            "/v1/order/dca",
            "/v1/order/solver",
            "/v1/order/{order_hash}",
            "/v1/order/cancel",
            "/v1/orders/tx/{tx_hash}",
            "/v1/orders/{address}",
            "/v1/trades/tx/{tx_hash}",
            "/v1/trades/{address}",
        ];

        for path in &expected_paths {
            assert!(
                paths.contains_key(*path),
                "Missing path: {path}. Found: {:?}",
                paths.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(paths.len(), expected_paths.len());
    }

    #[test]
    fn test_openapi_json_contains_all_schemas() {
        let client = client();
        let spec = get_openapi_json(&client);
        let schemas = spec["components"]["schemas"].as_object().unwrap();

        let expected_schemas = [
            "ApiErrorResponse",
            "ApiErrorDetail",
            "HealthResponse",
            "TokenRef",
            "Approval",
            "TokenInfo",
            "TokenListResponse",
            "SwapQuoteRequest",
            "SwapQuoteResponse",
            "SwapCalldataRequest",
            "SwapCalldataResponse",
            "PeriodUnit",
            "OrderType",
            "DeployDcaOrderRequest",
            "DeploySolverOrderRequest",
            "DeployOrderResponse",
            "CancelOrderRequest",
            "CancelOrderResponse",
            "CancelTransaction",
            "CancelSummary",
            "TokenReturn",
            "OrderDetailsInfo",
            "OrderTradeEntry",
            "OrderDetail",
            "OrderSummary",
            "OrdersPagination",
            "OrdersListResponse",
            "OrderByTxEntry",
            "OrdersByTxResponse",
            "TradeByAddress",
            "TradesPagination",
            "TradesByAddressResponse",
            "TradeRequest",
            "TradeResult",
            "TradeByTxEntry",
            "TradesTotals",
            "TradesByTxResponse",
        ];

        for schema in &expected_schemas {
            assert!(
                schemas.contains_key(*schema),
                "Missing schema: {schema}. Found: {:?}",
                schemas.keys().collect::<Vec<_>>()
            );
        }
    }
}
