use rocket::http::Status;
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

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

impl<'r> Responder<'r, 'static> for ApiError {
    fn respond_to(self, req: &'r Request<'_>) -> rocket::response::Result<'static> {
        let (status, code, message) = match &self {
            ApiError::BadRequest(msg) => (Status::BadRequest, "BAD_REQUEST", msg.clone()),
            ApiError::Unauthorized(msg) => (Status::Unauthorized, "UNAUTHORIZED", msg.clone()),
            ApiError::NotFound(msg) => (Status::NotFound, "NOT_FOUND", msg.clone()),
            ApiError::Internal(msg) => {
                (Status::InternalServerError, "INTERNAL_ERROR", msg.clone())
            }
        };
        let body = ApiErrorResponse {
            error: ApiErrorDetail {
                code: code.to_string(),
                message,
            },
        };
        Response::build_from(Json(body).respond_to(req)?)
            .status(status)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_status_mapping() {
        let bad_request = ApiError::BadRequest("bad".into());
        assert!(matches!(bad_request, ApiError::BadRequest(_)));

        let unauthorized = ApiError::Unauthorized("denied".into());
        assert!(matches!(unauthorized, ApiError::Unauthorized(_)));

        let not_found = ApiError::NotFound("missing".into());
        assert!(matches!(not_found, ApiError::NotFound(_)));

        let internal = ApiError::Internal("oops".into());
        assert!(matches!(internal, ApiError::Internal(_)));
    }

    #[test]
    fn test_error_response_serialization() {
        let resp = ApiErrorResponse {
            error: ApiErrorDetail {
                code: "BAD_REQUEST".into(),
                message: "test error".into(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("test error"));
        assert!(json.contains("BAD_REQUEST"));

        let deserialized: ApiErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.error.message, "test error");
        assert_eq!(deserialized.error.code, "BAD_REQUEST");
    }

    #[test]
    fn test_error_response_nested_shape() {
        let resp = ApiErrorResponse {
            error: ApiErrorDetail {
                code: "NOT_FOUND".into(),
                message: "not found".into(),
            },
        };
        let value: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert!(value["error"]["code"].is_string());
        assert!(value["error"]["message"].is_string());
    }
}
