use crate::db::DbPool;
use crate::error::ApiError;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::Engine;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::Request;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: i64,
    pub key_id: String,
    pub secret_hash: String,
    pub label: String,
    pub owner: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub struct AuthenticatedKey {
    pub id: i64,
    pub key_id: String,
    pub label: String,
    pub owner: String,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthenticatedKey {
    type Error = ApiError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let pool = match req.rocket().state::<DbPool>() {
            Some(p) => p,
            None => {
                tracing::error!("DbPool not found in managed state");
                return Outcome::Error((
                    Status::InternalServerError,
                    ApiError::Internal("database unavailable".into()),
                ));
            }
        };

        let header = match req.headers().get_one("Authorization") {
            Some(h) => h,
            None => {
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("missing Authorization header".into()),
                ));
            }
        };

        let encoded = match header.strip_prefix("Basic ") {
            Some(e) => e,
            None => {
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("invalid Authorization scheme".into()),
                ));
            }
        };

        let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(d) => d,
            Err(_) => {
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("invalid base64 encoding".into()),
                ));
            }
        };

        let credentials = match String::from_utf8(decoded) {
            Ok(s) => s,
            Err(_) => {
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("invalid credentials encoding".into()),
                ));
            }
        };

        let (key_id, secret) = match credentials.split_once(':') {
            Some(pair) => pair,
            None => {
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("invalid credentials format".into()),
                ));
            }
        };

        let row: Option<ApiKeyRow> = match sqlx::query_as::<_, ApiKeyRow>(
            "SELECT id, key_id, secret_hash, label, owner, active, created_at, updated_at \
             FROM api_keys WHERE key_id = ? AND active = 1",
        )
        .bind(key_id)
        .fetch_optional(pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "database error during auth lookup");
                return Outcome::Error((
                    Status::InternalServerError,
                    ApiError::Internal("authentication check failed".into()),
                ));
            }
        };

        let row = match row {
            Some(r) => r,
            None => {
                tracing::warn!(key_id = %key_id, "API key not found or inactive");
                return Outcome::Error((
                    Status::Unauthorized,
                    ApiError::Unauthorized("invalid credentials".into()),
                ));
            }
        };

        let parsed_hash = match PasswordHash::new(&row.secret_hash) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, key_id = %key_id, "failed to parse stored hash");
                return Outcome::Error((
                    Status::InternalServerError,
                    ApiError::Internal("authentication check failed".into()),
                ));
            }
        };

        if Argon2::default()
            .verify_password(secret.as_bytes(), &parsed_hash)
            .is_err()
        {
            tracing::warn!(key_id = %key_id, "invalid secret");
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid credentials".into()),
            ));
        }

        tracing::info!(key_id = %row.key_id, label = %row.label, "authenticated");

        Outcome::Success(AuthenticatedKey {
            id: row.id,
            key_id: row.key_id,
            label: row.label,
            owner: row.owner,
        })
    }
}

pub fn hash_secret(secret: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(secret.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify_secret() {
        let secret = "test-secret-123";
        let hash = hash_secret(secret).expect("hash");
        let parsed = PasswordHash::new(&hash).expect("parse");
        assert!(Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .is_ok());
    }

    #[test]
    fn test_wrong_secret_fails_verification() {
        let hash = hash_secret("correct-secret").expect("hash");
        let parsed = PasswordHash::new(&hash).expect("parse");
        assert!(Argon2::default()
            .verify_password(b"wrong-secret", &parsed)
            .is_err());
    }
}
