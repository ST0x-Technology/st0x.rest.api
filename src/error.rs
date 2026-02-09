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
