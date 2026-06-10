mod migrate;
mod pool;
pub(crate) mod registry_history;
pub(crate) mod wrapped_exchange_rate_history;

pub type DbPool = sqlx::Pool<sqlx::Sqlite>;

pub async fn init(database_url: &str, max_connections: u32) -> Result<DbPool, sqlx::Error> {
    let pool = pool::create(database_url, max_connections).await?;
    migrate::run(&pool).await?;
    Ok(pool)
}
