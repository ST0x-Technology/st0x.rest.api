use super::DbPool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;
use std::time::Duration;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn create(
    database_url: &str,
    max_connections: u32,
) -> Result<DbPool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    tracing::info!(
        database_url = %database_url,
        max_connections,
        "database pool created"
    );

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_a_concurrent_writer_instead_of_failing_busy() {
        let file = tempfile::NamedTempFile::new().expect("database file");
        let database_url = format!("sqlite://{}", file.path().display());
        let pool = create(&database_url, 2).await.expect("database pool");
        sqlx::query("CREATE TABLE items (value INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .expect("create table");

        let mut first_writer = pool.begin().await.expect("first transaction");
        sqlx::query("INSERT INTO items (value) VALUES (1)")
            .execute(&mut *first_writer)
            .await
            .expect("first write");

        let waiting_pool = pool.clone();
        let waiting_writer = tokio::spawn(async move {
            sqlx::query("INSERT INTO items (value) VALUES (2)")
                .execute(&waiting_pool)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting_writer.is_finished());

        first_writer.commit().await.expect("release write lock");
        waiting_writer
            .await
            .expect("waiting writer task")
            .expect("waiting write");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
            .fetch_one(&pool)
            .await
            .expect("row count");
        assert_eq!(count, 2);
    }
}
