use super::DbPool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RegistryUrlHistoryRow {
    pub id: i64,
    pub previous_url: String,
    pub new_url: String,
    pub actor_key_id: String,
    pub actor_label: String,
    pub actor_owner: String,
    pub changed_at: String,
}

pub(crate) struct NewRegistryUrlHistory<'a> {
    pub previous_url: &'a str,
    pub new_url: &'a str,
    pub actor_key_id: &'a str,
    pub actor_label: &'a str,
    pub actor_owner: &'a str,
}

pub(crate) async fn insert_registry_url_change(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    change: &NewRegistryUrlHistory<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO registry_url_history \
         (previous_url, new_url, actor_key_id, actor_label, actor_owner) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(change.previous_url)
    .bind(change.new_url)
    .bind(change.actor_key_id)
    .bind(change.actor_label)
    .bind(change.actor_owner)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub(crate) async fn list_registry_url_history(
    pool: &DbPool,
) -> Result<Vec<RegistryUrlHistoryRow>, sqlx::Error> {
    sqlx::query_as::<_, RegistryUrlHistoryRow>(
        "SELECT id, previous_url, new_url, actor_key_id, actor_label, actor_owner, changed_at \
         FROM registry_url_history \
         ORDER BY changed_at DESC, id DESC",
    )
    .fetch_all(pool)
    .await
}
