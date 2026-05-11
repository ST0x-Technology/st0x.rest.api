use super::DbPool;

pub(crate) const VALIDATION_STATUS_SUCCESS: &str = "success";
pub(crate) const VALIDATION_STATUS_FAILED: &str = "failed";

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PrivateRegistryHistoryRow {
    pub source_commit: String,
    pub payload_sha256: String,
    pub actor_key_id: String,
    pub actor_label: String,
    pub actor_owner: String,
    pub validation_status: String,
    pub validation_error: Option<String>,
    pub changed_at: String,
}

pub(crate) struct NewPrivateRegistryHistory<'a> {
    pub source_commit: &'a str,
    pub payload_sha256: &'a str,
    pub actor_key_id: &'a str,
    pub actor_label: &'a str,
    pub actor_owner: &'a str,
    pub validation_status: &'a str,
    pub validation_error: Option<&'a str>,
}

pub(crate) async fn insert_private_registry_change(
    pool: &DbPool,
    change: &NewPrivateRegistryHistory<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO private_registry_history \
         (source_commit, payload_sha256, actor_key_id, actor_label, actor_owner, validation_status, validation_error) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(change.source_commit)
    .bind(change.payload_sha256)
    .bind(change.actor_key_id)
    .bind(change.actor_label)
    .bind(change.actor_owner)
    .bind(change.validation_status)
    .bind(change.validation_error)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn list_private_registry_history(
    pool: &DbPool,
) -> Result<Vec<PrivateRegistryHistoryRow>, sqlx::Error> {
    sqlx::query_as::<_, PrivateRegistryHistoryRow>(
        "SELECT source_commit, payload_sha256, actor_key_id, actor_label, actor_owner, validation_status, validation_error, changed_at \
         FROM private_registry_history \
         ORDER BY changed_at DESC, id DESC",
    )
    .fetch_all(pool)
    .await
}

pub(crate) async fn latest_successful_private_registry(
    pool: &DbPool,
) -> Result<Option<PrivateRegistryHistoryRow>, sqlx::Error> {
    sqlx::query_as::<_, PrivateRegistryHistoryRow>(
        "SELECT source_commit, payload_sha256, actor_key_id, actor_label, actor_owner, validation_status, validation_error, changed_at \
         FROM private_registry_history \
         WHERE validation_status = ? \
         ORDER BY changed_at DESC, id DESC \
         LIMIT 1",
    )
    .bind(VALIDATION_STATUS_SUCCESS)
    .fetch_optional(pool)
    .await
}
