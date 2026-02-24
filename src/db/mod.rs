mod migrate;
mod pool;
pub(crate) mod settings;

pub type DbPool = sqlx::Pool<sqlx::Sqlite>;

pub async fn init(database_url: &str) -> Result<DbPool, sqlx::Error> {
    let pool = pool::create(database_url).await?;
    migrate::run(&pool).await?;
    Ok(pool)
}
