use crate::db::DbPool;
use crate::error::ApiError;
use crate::fairings::rate_limiter::CachedRateLimitInfo;
use crate::fairings::RateLimiter;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::Engine;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::Request;
use std::sync::Mutex;

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

pub struct AuthKeyId(pub Option<i64>);

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
        let Some(pool) = req.rocket().state::<DbPool>() else {
            tracing::error!("DbPool not found in managed state");
            return Outcome::Error((
                Status::InternalServerError,
                ApiError::Internal("database unavailable".into()),
            ));
        };

        let Some(header) = req.headers().get_one("Authorization") else {
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("missing Authorization header".into()),
            ));
        };

        const BASIC_PREFIX: &str = "Basic ";
        if header.len() < BASIC_PREFIX.len()
            || !header[..BASIC_PREFIX.len()].eq_ignore_ascii_case(BASIC_PREFIX)
        {
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid Authorization scheme".into()),
            ));
        }
        let encoded = &header[BASIC_PREFIX.len()..];

        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid base64 encoding".into()),
            ));
        };

        let Ok(credentials) = String::from_utf8(decoded) else {
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid credentials encoding".into()),
            ));
        };

        let Some((key_id, secret)) = credentials.split_once(':') else {
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid credentials format".into()),
            ));
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

        let Some(row) = row else {
            tracing::warn!(key_id = %key_id, "API key not found or inactive");
            return Outcome::Error((
                Status::Unauthorized,
                ApiError::Unauthorized("invalid credentials".into()),
            ));
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

        req.local_cache(|| AuthKeyId(Some(row.id)));

        let rl = match req.rocket().state::<RateLimiter>() {
            Some(rl) => rl,
            None => {
                tracing::error!("RateLimiter not found in managed state");
                return Outcome::Error((
                    Status::InternalServerError,
                    ApiError::Internal("rate limiter unavailable".into()),
                ));
            }
        };

        match rl.check_per_key(row.id) {
            Ok((true, info)) => {
                if let Some(info) = info {
                    let cache = req.local_cache(|| CachedRateLimitInfo(Mutex::new(None)));
                    if let Ok(mut guard) = cache.0.lock() {
                        *guard = Some(info);
                    }
                }
            }
            Ok((false, info)) => {
                if let Some(info) = info {
                    let cache = req.local_cache(|| CachedRateLimitInfo(Mutex::new(None)));
                    if let Ok(mut guard) = cache.0.lock() {
                        *guard = Some(info);
                    }
                }
                tracing::warn!(key_id = %row.key_id, "per-key rate limit exceeded");
                return Outcome::Error((
                    Status::TooManyRequests,
                    ApiError::RateLimited("Too many requests, please try again later".into()),
                ));
            }
            Err(e) => {
                tracing::error!(key_id = %row.key_id, error = %e, "per-key rate limiter failed");
                return Outcome::Error((Status::InternalServerError, e));
            }
        }

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
