use crate::error::{ApiErrorDetail, ApiErrorResponse};
use crate::fairings::request_span_for;
use rocket::serde::json::Json;
use rocket::Catcher;
use rocket::Request;

#[catch(400)]
pub fn bad_request(req: &Request<'_>) -> Json<ApiErrorResponse> {
    let span = request_span_for(req);
    span.in_scope(|| {
        tracing::warn!("bad request (invalid content type, missing headers, or malformed input)")
    });

    Json(ApiErrorResponse {
        error: ApiErrorDetail {
            code: "BAD_REQUEST".to_string(),
            message: "The request was invalid or malformed".to_string(),
        },
    })
}

#[catch(401)]
pub fn unauthorized(req: &Request<'_>) -> Json<ApiErrorResponse> {
    let span = request_span_for(req);
    span.in_scope(|| tracing::warn!("unauthorized (missing or invalid credentials)"));

    Json(ApiErrorResponse {
        error: ApiErrorDetail {
            code: "UNAUTHORIZED".to_string(),
            message: "Missing or invalid credentials".to_string(),
        },
    })
}

#[catch(404)]
pub fn not_found(req: &Request<'_>) -> Json<ApiErrorResponse> {
    let span = request_span_for(req);
    span.in_scope(|| tracing::warn!("route not found"));

    Json(ApiErrorResponse {
        error: ApiErrorDetail {
            code: "NOT_FOUND".to_string(),
            message: "The requested resource was not found".to_string(),
        },
    })
}

#[catch(422)]
pub fn unprocessable_entity(req: &Request<'_>) -> Json<ApiErrorResponse> {
    let span = request_span_for(req);
    span.in_scope(|| tracing::warn!("unprocessable entity (likely malformed request body)"));

    Json(ApiErrorResponse {
        error: ApiErrorDetail {
            code: "UNPROCESSABLE_ENTITY".to_string(),
            message: "Request body could not be parsed".to_string(),
        },
    })
}

#[catch(500)]
pub fn internal_server_error(req: &Request<'_>) -> Json<ApiErrorResponse> {
    let span = request_span_for(req);
    span.in_scope(|| tracing::error!("unhandled internal server error"));

    Json(ApiErrorResponse {
        error: ApiErrorDetail {
            code: "INTERNAL_ERROR".to_string(),
            message: "Internal server error".to_string(),
        },
    })
}

pub fn catchers() -> Vec<Catcher> {
    rocket::catchers![
        bad_request,
        unauthorized,
        not_found,
        unprocessable_entity,
        internal_server_error
    ]
}
