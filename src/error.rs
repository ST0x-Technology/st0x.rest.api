use crate::fairings::request_span_for;
use rocket::http::{Header, Status};
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::{Request, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiErrorDetail {
    #[schema(example = "BAD_REQUEST")]
    pub code: String,
    #[schema(example = "Something went wrong")]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({"error": {"code": "BAD_REQUEST", "message": "Something went wrong"}}))]
pub struct ApiErrorResponse {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiError {
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
}

impl<'r> Responder<'r, 'static> for ApiError {
    fn respond_to(self, req: &'r Request<'_>) -> rocket::response::Result<'static> {
        let (status, code, message) = match &self {
            ApiError::BadRequest(msg) => (Status::BadRequest, "BAD_REQUEST", msg.clone()),
            ApiError::Unauthorized(msg) => (Status::Unauthorized, "UNAUTHORIZED", msg.clone()),
            ApiError::NotFound(msg) => (Status::NotFound, "NOT_FOUND", msg.clone()),
            ApiError::Internal(msg) => (Status::InternalServerError, "INTERNAL_ERROR", msg.clone()),
            ApiError::RateLimited(msg) => (Status::TooManyRequests, "RATE_LIMITED", msg.clone()),
        };
        let span = request_span_for(req);
        span.in_scope(|| {
            if status.code >= 500 {
                tracing::error!(
                    status = status.code,
                    code = %code,
                    error_message = %message,
                    "request failed"
                );
            } else {
                tracing::warn!(
                    status = status.code,
                    code = %code,
                    error_message = %message,
                    "request failed"
                );
            }
        });

        let body = ApiErrorResponse {
            error: ApiErrorDetail {
                code: code.to_string(),
                message,
            },
        };
        let json_response = match Json(body).respond_to(req) {
            Ok(r) => r,
            Err(s) => {
                tracing::error!(status = %s.code, "failed to serialize error response");
                return Err(s);
            }
        };
        let mut response = Response::build_from(json_response)
            .status(status)
            .finalize();
        if matches!(self, ApiError::RateLimited(_)) {
            response.set_header(Header::new("Retry-After", "60"));
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::local::blocking::Client;

    #[get("/bad-request")]
    fn bad_request() -> Result<(), ApiError> {
        Err(ApiError::BadRequest("invalid input".into()))
    }
    #[get("/unauthorized")]
    fn unauthorized() -> Result<(), ApiError> {
        Err(ApiError::Unauthorized("no token".into()))
    }
    #[get("/not-found")]
    fn not_found() -> Result<(), ApiError> {
        Err(ApiError::NotFound("order not found".into()))
    }
    #[get("/internal")]
    fn internal() -> Result<(), ApiError> {
        Err(ApiError::Internal("something broke".into()))
    }

    fn error_client() -> Client {
        let rocket = rocket::build().mount(
            "/",
            rocket::routes![bad_request, unauthorized, not_found, internal],
        );
        Client::tracked(rocket).expect("valid rocket instance")
    }

    fn assert_error_response(
        client: &Client,
        path: &str,
        expected_status: u16,
        expected_code: &str,
        expected_message: &str,
    ) {
        let response = client.get(path).dispatch();
        assert_eq!(response.status().code, expected_status);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], expected_code);
        assert_eq!(body["error"]["message"], expected_message);
    }

    #[test]
    fn test_bad_request_returns_400() {
        let client = error_client();
        assert_error_response(&client, "/bad-request", 400, "BAD_REQUEST", "invalid input");
    }

    #[test]
    fn test_unauthorized_returns_401() {
        let client = error_client();
        assert_error_response(&client, "/unauthorized", 401, "UNAUTHORIZED", "no token");
    }

    #[test]
    fn test_not_found_returns_404() {
        let client = error_client();
        assert_error_response(&client, "/not-found", 404, "NOT_FOUND", "order not found");
    }

    #[test]
    fn test_internal_returns_500() {
        let client = error_client();
        assert_error_response(
            &client,
            "/internal",
            500,
            "INTERNAL_ERROR",
            "something broke",
        );
    }
}
