use crate::error::ApiError;
use crate::raindex::RaindexProvider;
use crate::test_helpers::mock_raindex_config;
use rocket::http::Status;
use rocket::local::asynchronous::Client;
use rocket::State;

#[get("/shared-client")]
async fn shared_client_contract(
    provider: &State<RaindexProvider>,
) -> Result<&'static str, ApiError> {
    let orderbook_address =
        alloy::primitives::address!("0xd2938e7c9fe3597f78832ce780feb61945c377d7");

    provider
        .client()
        .get_orderbook_client(orderbook_address)
        .map(|_| "ok")
        .map_err(|e| ApiError::Internal(format!("{e}")))
}

#[rocket::async_test]
async fn test_shared_client_succeeds_with_valid_registry() {
    let raindex_config = mock_raindex_config().await;
    let rocket = rocket::build()
        .manage(raindex_config)
        .mount("/__test", rocket::routes![shared_client_contract]);
    let client = Client::tracked(rocket).await.expect("valid test client");

    let response = client.get("/__test/shared-client").dispatch().await;

    assert_eq!(response.status(), Status::Ok);
    let body = response.into_string().await.expect("response body");
    assert_eq!(body, "ok");
}
