use crate::auth::AdminKey;
use crate::error::ApiError;
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::log_files::LogFiles;
use chrono::NaiveDate;
use rocket::form::FromForm;
use rocket::fs::NamedFile;
use rocket::http::{ContentType, Header};
use rocket::request::Request;
use rocket::response::Responder;
use rocket::{Route, State};
use tracing::Instrument;

#[derive(Debug, Clone, FromForm)]
pub struct AdminLogDownloadParams {
    pub date: String,
}

pub struct AdminLogDownload {
    file: NamedFile,
    filename: String,
}

impl<'r> Responder<'r, 'static> for AdminLogDownload {
    fn respond_to(self, req: &'r Request<'_>) -> rocket::response::Result<'static> {
        let mut response = self.file.respond_to(req)?;
        response.set_header(ContentType::Binary);
        response.set_header(Header::new(
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", self.filename),
        ));
        Ok(response)
    }
}

fn parse_log_date(date: &str) -> Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| ApiError::BadRequest("date must use YYYY-MM-DD format".into()))
}

#[get("/logs?<params..>")]
pub async fn get_logs(
    _global: GlobalRateLimit,
    _admin: AdminKey,
    log_files: &State<LogFiles>,
    span: TracingSpan,
    params: AdminLogDownloadParams,
) -> Result<AdminLogDownload, ApiError> {
    async move {
        tracing::info!(date = %params.date, "request received");

        let date = parse_log_date(&params.date)?;
        let log_file = log_files.file_for_date(date);
        let log_path = log_file.path().to_path_buf();

        tracing::info!(path = %log_path.display(), "resolved log file path");

        let file = NamedFile::open(&log_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                tracing::warn!(path = %log_path.display(), "requested log file not found");
                ApiError::NotFound("log file not found".into())
            } else {
                tracing::error!(path = %log_path.display(), error = %e, "failed to open log file");
                ApiError::Internal("failed to open log file".into())
            }
        })?;

        tracing::info!(
            path = %log_path.display(),
            filename = %log_file.download_filename(),
            "serving log file"
        );

        Ok(AdminLogDownload {
            file,
            filename: log_file.download_filename().to_string(),
        })
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_logs]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{basic_auth_header, seed_admin_key, seed_api_key, TestClientBuilder};
    use rocket::http::{ContentType, Header, Status};
    use tempfile::TempDir;

    fn write_log_file(temp_dir: &TempDir, date: &str, contents: &str) {
        let path = temp_dir.path().join(format!("st0x-rest-api.log.{date}"));
        std::fs::write(path, contents).expect("write log file");
    }

    #[rocket::async_test]
    async fn test_get_logs_with_admin_key() {
        let temp_dir = TempDir::new().expect("temp dir");
        write_log_file(&temp_dir, "2026-03-13", "line one\nline two\n");

        let client = TestClientBuilder::new()
            .log_dir(temp_dir.path())
            .build()
            .await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/admin/logs?date=2026-03-13")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        assert_eq!(response.content_type(), Some(ContentType::Binary));
        assert_eq!(
            response.headers().get_one("Content-Disposition"),
            Some("attachment; filename=\"st0x-rest-api-2026-03-13.log\"")
        );
        assert_eq!(
            response.into_string().await.unwrap(),
            "line one\nline two\n".to_string()
        );
    }

    #[rocket::async_test]
    async fn test_get_logs_with_non_admin_key_returns_403() {
        let temp_dir = TempDir::new().expect("temp dir");
        write_log_file(&temp_dir, "2026-03-13", "line one\n");

        let client = TestClientBuilder::new()
            .log_dir(temp_dir.path())
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/admin/logs?date=2026-03-13")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Forbidden);
    }

    #[rocket::async_test]
    async fn test_get_logs_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;

        let response = client.get("/admin/logs?date=2026-03-13").dispatch().await;

        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_logs_with_invalid_date_returns_400() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/admin/logs?date=2026-13-40")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::BadRequest);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "BAD_REQUEST");
        assert_eq!(body["error"]["message"], "date must use YYYY-MM-DD format");
    }

    #[rocket::async_test]
    async fn test_get_logs_when_file_is_missing_returns_404() {
        let temp_dir = TempDir::new().expect("temp dir");
        let client = TestClientBuilder::new()
            .log_dir(temp_dir.path())
            .build()
            .await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/admin/logs?date=2026-03-13")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NotFound);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "NOT_FOUND");
        assert_eq!(body["error"]["message"], "log file not found");
    }
}
