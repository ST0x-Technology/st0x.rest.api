use base64::Engine;
use rocket::local::asynchronous::Client;

pub(crate) async fn client() -> Client {
    let id = uuid::Uuid::new_v4();
    let pool = crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
        .await
        .expect("database init");
    let rate_limiter = crate::fairings::RateLimiter::new(10000, 10000);
    Client::tracked(crate::rocket(pool, rate_limiter).expect("valid rocket instance"))
        .await
        .expect("valid client")
}

pub(crate) async fn seed_api_key(client: &Client) -> (String, String) {
    let key_id = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let hash = crate::auth::hash_secret(&secret).expect("hash secret");

    let pool = client
        .rocket()
        .state::<crate::db::DbPool>()
        .expect("pool in state");
    sqlx::query("INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, ?, ?, ?)")
        .bind(&key_id)
        .bind(&hash)
        .bind("test-key")
        .bind("test-owner")
        .execute(pool)
        .await
        .expect("insert api key");

    (key_id, secret)
}

pub(crate) fn basic_auth_header(key_id: &str, secret: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{key_id}:{secret}"));
    format!("Basic {encoded}")
}
