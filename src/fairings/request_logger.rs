use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Header;
use rocket::request::{FromRequest, Outcome};
use rocket::{Data, Request, Response};
use std::time::Instant;
use uuid::Uuid;

struct RequestMeta {
    start: Instant,
    request_id: String,
    span: tracing::Span,
}

pub struct RequestLogger;
pub struct TracingSpan(pub tracing::Span);

const REQUEST_ID_HEADER: &str = "X-Request-Id";

fn fallback_meta() -> RequestMeta {
    RequestMeta {
        start: Instant::now(),
        request_id: "unknown".to_string(),
        span: tracing::Span::none(),
    }
}

fn extract_request_id(req: &Request<'_>) -> String {
    match req.headers().get_one(REQUEST_ID_HEADER) {
        Some(value) => {
            let trimmed = value.trim();
            if !trimmed.is_empty()
                && trimmed.len() <= 128
                && trimmed.is_ascii()
                && !trimmed.chars().any(|c| c.is_control())
            {
                return trimmed.to_string();
            }
            Uuid::new_v4().to_string()
        }
        None => Uuid::new_v4().to_string(),
    }
}

pub(crate) fn request_span_for(req: &Request<'_>) -> tracing::Span {
    req.local_cache(fallback_meta).span.clone()
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for TracingSpan {
    type Error = ();

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(TracingSpan(request_span_for(req)))
    }
}

#[rocket::async_trait]
impl Fairing for RequestLogger {
    fn info(&self) -> Info {
        Info {
            name: "Request Logger",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, req: &mut Request<'_>, _data: &mut Data<'_>) {
        let request_id = extract_request_id(req);
        let span = tracing::info_span!(
            "request",
            method = %req.method(),
            uri = %req.uri(),
            request_id = %request_id,
        );
        span.in_scope(|| tracing::info!("request started"));
        req.local_cache(|| RequestMeta {
            start: Instant::now(),
            request_id,
            span,
        });
    }

    async fn on_response<'r>(&self, req: &'r Request<'_>, res: &mut Response<'r>) {
        let meta = req.local_cache(fallback_meta);
        let duration_ms = meta.start.elapsed().as_secs_f64() * 1000.0;
        let status = res.status().code;

        meta.span.in_scope(|| {
            if status >= 500 {
                tracing::error!(status, duration_ms, "request completed");
            } else if status >= 400 {
                tracing::warn!(status, duration_ms, "request completed");
            } else {
                tracing::info!(status, duration_ms, "request completed");
            }
        });

        res.set_header(Header::new(REQUEST_ID_HEADER, meta.request_id.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::http::Status;
    use rocket::local::blocking::Client;
    use tracing_test::traced_test;

    #[get("/test")]
    fn test_route() -> &'static str {
        "ok"
    }

    fn client() -> Client {
        let rocket = rocket::build()
            .mount("/", rocket::routes![test_route])
            .attach(RequestLogger);
        Client::tracked(rocket).expect("valid rocket instance")
    }

    #[test]
    fn generates_request_id_when_none_provided() {
        let client = client();
        let response = client.get("/test").dispatch();
        assert_eq!(response.status(), Status::Ok);
        let id = response.headers().get_one(REQUEST_ID_HEADER);
        assert!(id.is_some());
        assert!(Uuid::parse_str(id.unwrap()).is_ok());
    }

    #[test]
    fn echoes_valid_client_request_id() {
        let client = client();
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, "my-custom-id-123"))
            .dispatch();
        assert_eq!(
            response.headers().get_one(REQUEST_ID_HEADER),
            Some("my-custom-id-123")
        );
    }

    #[test]
    fn rejects_empty_request_id() {
        let client = client();
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, ""))
            .dispatch();
        let id = response.headers().get_one(REQUEST_ID_HEADER).unwrap();
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn rejects_oversized_request_id() {
        let client = client();
        let long_id = "a".repeat(129);
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, long_id))
            .dispatch();
        let id = response.headers().get_one(REQUEST_ID_HEADER).unwrap();
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn rejects_non_ascii_request_id() {
        let client = client();
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, "id-\u{00e9}-test"))
            .dispatch();
        let id = response.headers().get_one(REQUEST_ID_HEADER).unwrap();
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn rejects_control_char_request_id() {
        let client = client();
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, "id\x00test"))
            .dispatch();
        let id = response.headers().get_one(REQUEST_ID_HEADER).unwrap();
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn accepts_max_length_request_id() {
        let client = client();
        let max_id = "a".repeat(128);
        let response = client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, max_id.clone()))
            .dispatch();
        assert_eq!(
            response.headers().get_one(REQUEST_ID_HEADER),
            Some(max_id.as_str())
        );
    }

    #[traced_test]
    #[test]
    fn logs_request_lifecycle() {
        let client = client();
        client.get("/test").dispatch();
        assert!(logs_contain("request started"));
        assert!(logs_contain("request completed"));
    }

    #[traced_test]
    #[test]
    fn logs_contain_request_id_field() {
        let client = client();
        client
            .get("/test")
            .header(Header::new(REQUEST_ID_HEADER, "trace-me-123"))
            .dispatch();
        assert!(logs_contain("trace-me-123"));
    }
}
