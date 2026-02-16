use crate::error::ApiError;
use crate::raindex::RaindexProvider;
use crate::test_helpers::mock_invalid_raindex_config;
use rocket::http::Status;
use rocket::local::asynchronous::Client;
use rocket::State;

#[get("/raindex-client")]
async fn get_raindex_client_contract(
    provider: &State<RaindexProvider>,
) -> Result<&'static str, ApiError> {
    let _client = provider.get_raindex_client().map_err(ApiError::from)?;
    Ok("ok")
}

#[rocket::async_test]
async fn test_raindex_client_contract_route_returns_api_error_when_creation_fails() {
    let raindex_config = mock_invalid_raindex_config().await;
    let rocket = rocket::build()
        .manage(raindex_config)
        .mount("/__test", rocket::routes![get_raindex_client_contract]);
    let client = Client::tracked(rocket).await.expect("valid test client");

    let response = client.get("/__test/raindex-client").dispatch().await;

    assert_eq!(response.status(), Status::InternalServerError);
    let body: serde_json::Value = serde_json::from_str(
        &response
            .into_string()
            .await
            .expect("response should contain a JSON body"),
    )
    .expect("response body should be valid JSON");
    assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    assert_eq!(
        body["error"]["message"],
        "failed to initialize orderbook client"
    );
}
