//! Persistence for MCP grants.
//!
//! A grant is the unit of agent authorization: a label, a set of hosts, and a
//! token the holder presents on every request. Only the token's SHA-256 is
//! stored — see `migrations/0020_mcp_grants.sql` for why that digest lives in a
//! plain column rather than the encrypted keystore.

use serde::Serialize;
use sqlx::{Row, SqlitePool};

use crate::errors::{LumaError, Result};
use crate::ssh;

const MAX_LABEL_LENGTH: usize = 128;
/// A grant that reached this many hosts would be indistinguishable from
/// "everything", which defeats the point of scoping it.
const MAX_HOSTS_PER_GRANT: usize = 64;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpGrant {
    pub id: String,
    pub label: String,
    /// Leading characters of the token, for telling grants apart in the UI.
    /// Never the token itself.
    pub token_prefix: String,
    pub require_approval: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub host_ids: Vec<String>,
}

fn validate_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() || label.len() > MAX_LABEL_LENGTH || label.contains('\0') {
        return Err(LumaError::InvalidInput(format!(
            "grant label must be 1-{MAX_LABEL_LENGTH} characters"
        )));
    }
    Ok(label.to_string())
}

fn validate_host_ids(host_ids: &[String]) -> Result<()> {
    if host_ids.len() > MAX_HOSTS_PER_GRANT {
        return Err(LumaError::InvalidInput(format!(
            "a grant may reference at most {MAX_HOSTS_PER_GRANT} hosts"
        )));
    }
    for host_id in host_ids {
        ssh::validate_host_id(host_id)?;
    }
    Ok(())
}

async fn host_ids_for(pool: &SqlitePool, grant_id: &str) -> Result<Vec<String>> {
    let rows =
        sqlx::query("SELECT host_id FROM mcp_grant_hosts WHERE grant_id = ?1 ORDER BY host_id ASC")
            .bind(grant_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|row| row.get("host_id")).collect())
}

fn row_to_grant(row: &sqlx::sqlite::SqliteRow, host_ids: Vec<String>) -> McpGrant {
    McpGrant {
        id: row.get("id"),
        label: row.get("label"),
        token_prefix: row.get("token_prefix"),
        require_approval: row.get::<i64, _>("require_approval") != 0,
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
        host_ids,
    }
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<McpGrant>> {
    let rows = sqlx::query(
        "SELECT id, label, token_prefix, require_approval, created_at, last_used_at
         FROM mcp_grants ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;
    let mut grants = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.get("id");
        let host_ids = host_ids_for(pool, &id).await?;
        grants.push(row_to_grant(row, host_ids));
    }
    Ok(grants)
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<McpGrant> {
    let row = sqlx::query(
        "SELECT id, label, token_prefix, require_approval, created_at, last_used_at
         FROM mcp_grants WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| LumaError::InvalidInput("unknown grant".into()))?;
    let host_ids = host_ids_for(pool, id).await?;
    Ok(row_to_grant(&row, host_ids))
}

pub async fn count(pool: &SqlitePool) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS total FROM mcp_grants")
        .fetch_one(pool)
        .await?;
    Ok(row.get("total"))
}

pub async fn create(
    pool: &SqlitePool,
    label: &str,
    token_sha256: &[u8],
    token_prefix: &str,
    host_ids: &[String],
    require_approval: bool,
) -> Result<McpGrant> {
    let label = validate_label(label)?;
    validate_host_ids(host_ids)?;

    let id = uuid::Uuid::new_v4().to_string();
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO mcp_grants (id, label, token_sha256, token_prefix, require_approval)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(&id)
    .bind(&label)
    .bind(token_sha256)
    .bind(token_prefix)
    .bind(i64::from(require_approval))
    .execute(&mut *transaction)
    .await?;
    for host_id in host_ids {
        sqlx::query("INSERT OR IGNORE INTO mcp_grant_hosts (grant_id, host_id) VALUES (?1, ?2)")
            .bind(&id)
            .bind(host_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;

    get(pool, &id).await
}

pub async fn update(
    pool: &SqlitePool,
    id: &str,
    label: &str,
    host_ids: &[String],
    require_approval: bool,
) -> Result<McpGrant> {
    let label = validate_label(label)?;
    validate_host_ids(host_ids)?;

    let mut transaction = pool.begin().await?;
    let affected =
        sqlx::query("UPDATE mcp_grants SET label = ?2, require_approval = ?3 WHERE id = ?1")
            .bind(id)
            .bind(&label)
            .bind(i64::from(require_approval))
            .execute(&mut *transaction)
            .await?
            .rows_affected();
    if affected == 0 {
        return Err(LumaError::InvalidInput("unknown grant".into()));
    }
    sqlx::query("DELETE FROM mcp_grant_hosts WHERE grant_id = ?1")
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    for host_id in host_ids {
        sqlx::query("INSERT OR IGNORE INTO mcp_grant_hosts (grant_id, host_id) VALUES (?1, ?2)")
            .bind(id)
            .bind(host_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;

    get(pool, id).await
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<()> {
    let affected = sqlx::query("DELETE FROM mcp_grants WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(LumaError::InvalidInput("unknown grant".into()));
    }
    Ok(())
}

/// Look a grant up by the digest of a presented token.
///
/// The digest is the lookup key, so a wrong token costs one indexed miss and
/// never iterates grants.
pub async fn find_by_token_hash(
    pool: &SqlitePool,
    token_sha256: &[u8],
) -> Result<Option<McpGrant>> {
    let row = sqlx::query(
        "SELECT id, label, token_prefix, require_approval, created_at, last_used_at
         FROM mcp_grants WHERE token_sha256 = ?1",
    )
    .bind(token_sha256)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.get("id");
    let host_ids = host_ids_for(pool, &id).await?;
    Ok(Some(row_to_grant(&row, host_ids)))
}

pub async fn touch_last_used(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE mcp_grants SET last_used_at = unixepoch() WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hosts::{self, HostInput};

    async fn create_host(pool: &SqlitePool, name: &str) -> String {
        hosts::create(
            pool,
            HostInput {
                vault_id: crate::storage::vaults::default_id(),
                name: name.into(),
                hostname: "server.example.com".into(),
                port: 22,
                username: None,
                group_id: None,
                authentication_type: "interactive".into(),
                key_id: None,
                identity_id: None,
                proxy_jump_host_id: None,
                startup_command: None,
                working_directory: None,
                environment: None,
                tags: vec![],
                favorite: false,
                tab_color: None,
                transport: "ssh".into(),
                mosh_server_path: None,
                mosh_port_range: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn create_find_by_hash_and_delete() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let host_id = create_host(&pool, "Server").await;

        let grant = create(
            &pool,
            "  Claude Code  ",
            &[7u8; 32],
            "luma_mcp_abcd",
            std::slice::from_ref(&host_id),
            false,
        )
        .await
        .unwrap();
        assert_eq!(grant.label, "Claude Code");
        assert_eq!(grant.host_ids, vec![host_id.clone()]);
        assert!(!grant.require_approval);
        assert!(grant.last_used_at.is_none());

        let found = find_by_token_hash(&pool, &[7u8; 32]).await.unwrap();
        assert_eq!(
            found.as_ref().map(|g| g.id.as_str()),
            Some(grant.id.as_str())
        );
        assert_eq!(found.unwrap().host_ids, vec![host_id]);

        assert!(find_by_token_hash(&pool, &[8u8; 32])
            .await
            .unwrap()
            .is_none());

        assert_eq!(count(&pool).await.unwrap(), 1);
        delete(&pool, &grant.id).await.unwrap();
        assert_eq!(count(&pool).await.unwrap(), 0);
        assert!(find_by_token_hash(&pool, &[7u8; 32])
            .await
            .unwrap()
            .is_none());
        assert!(delete(&pool, &grant.id).await.is_err());
    }

    #[tokio::test]
    async fn update_replaces_scope_and_approval() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let first = create_host(&pool, "First").await;
        let second = create_host(&pool, "Second").await;

        let grant = create(&pool, "Agent", &[1u8; 32], "luma_mcp_1", &[first], true)
            .await
            .unwrap();
        assert!(grant.require_approval);

        let updated = update(
            &pool,
            &grant.id,
            "Renamed",
            std::slice::from_ref(&second),
            false,
        )
        .await
        .unwrap();
        assert_eq!(updated.label, "Renamed");
        assert_eq!(updated.host_ids, vec![second]);
        assert!(!updated.require_approval);

        assert!(update(&pool, "missing", "x", &[], false).await.is_err());
    }

    #[tokio::test]
    async fn deleting_a_host_narrows_the_grant_rather_than_dangling() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let kept = create_host(&pool, "Kept").await;
        let removed = create_host(&pool, "Removed").await;

        let grant = create(
            &pool,
            "Agent",
            &[2u8; 32],
            "luma_mcp_2",
            &[kept.clone(), removed.clone()],
            false,
        )
        .await
        .unwrap();
        assert_eq!(grant.host_ids.len(), 2);

        hosts::delete(&pool, &removed).await.unwrap();

        let reloaded = get(&pool, &grant.id).await.unwrap();
        assert_eq!(reloaded.host_ids, vec![kept]);
    }

    #[tokio::test]
    async fn rejects_bad_labels_and_host_ids() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        assert!(create(&pool, "   ", &[3u8; 32], "p", &[], false)
            .await
            .is_err());
        assert!(create(&pool, "bad\0label", &[3u8; 32], "p", &[], false)
            .await
            .is_err());
        assert!(
            create(&pool, "ok", &[3u8; 32], "p", &["../etc".into()], false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn touch_records_last_use() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let grant = create(&pool, "Agent", &[4u8; 32], "luma_mcp_4", &[], false)
            .await
            .unwrap();
        assert!(grant.last_used_at.is_none());

        touch_last_used(&pool, &grant.id).await.unwrap();
        assert!(get(&pool, &grant.id).await.unwrap().last_used_at.is_some());
    }
}
