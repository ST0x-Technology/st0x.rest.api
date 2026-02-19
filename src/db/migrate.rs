use super::DbPool;

pub(super) async fn run(pool: &DbPool) -> Result<(), sqlx::Error> {
    tracing::info!("running database migrations");
    sqlx::migrate!("./migrations").run(pool).await?;
    tracing::info!("database migrations complete");
    Ok(())
}
