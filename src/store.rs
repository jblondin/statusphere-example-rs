use std::str::FromStr;

use atrium_api::types::string::{Datetime, Did};
use atrium_common::store::Store;
use atrium_oauth::store::{
    session::{Session, SessionStore},
    state::{InternalStateData, StateStore},
};
use thiserror::Error;
use tower_sessions_sqlx_store::sqlx::{self, FromRow, SqlitePool};

#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "invalid table name '{0}': table names should start with an alphabetic character, \
        followed by alphanumeric and underscore characters"
    )]
    InvalidTableName(String),
    #[error("migration: {0}")]
    MigrationFailed(sqlx::Error),
    #[error("insert: {0}")]
    InsertFailed(sqlx::Error),
    #[error("select: {0}")]
    SelectFailed(sqlx::Error),
    #[error("delete: {0}")]
    DeleteFailed(sqlx::Error),
    #[error("delete all: {0}")]
    DeleteAllFailed(sqlx::Error),
    #[error("invalid did: {0}")]
    InvalidDid(&'static str),
    #[error("deserialization: {0}")]
    Deserialization(serde_json::Error),
    #[error("serialization: {0}")]
    Serialization(serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct Status {
    pub uri: String,
    pub author_did: Did,
    pub status: String,
    pub created_at: Datetime,
    pub indexed_at: Datetime,
}

// sqlx FromRow derive doesn't play nice with re-exported sqlx from tower_sessions_sqlx_store,
// so just implement it manually
// I probably should just import sqlx myself
impl<'a, R: sqlx::Row> FromRow<'a, R> for Status
where
    &'a str: sqlx::ColumnIndex<R>,
    String: sqlx::decode::Decode<'a, R::Database>,
    String: sqlx::types::Type<R::Database>,
{
    fn from_row(row: &'a R) -> Result<Self, sqlx::Error> {
        let uri: String = row.try_get("uri")?;
        let author_did: String = row.try_get("author_did")?;
        let status: String = row.try_get("status")?;
        let created_at: String = row.try_get("created_at")?;
        let indexed_at: String = row.try_get("indexed_at")?;
        Ok(Status {
            uri,
            author_did: Did::new(author_did)
                .map_err(|e| sqlx::Error::Decode(Box::new(Error::InvalidDid(e))))?,
            status,
            created_at: Datetime::from_str(created_at.as_str())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            indexed_at: Datetime::from_str(indexed_at.as_str())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StatusStore {
    pool: SqlitePool,
    table_name: String,
}

impl StatusStore {
    pub fn new(pool: SqlitePool, table_name: impl AsRef<str>) -> Result<Self, Error> {
        let table_name = table_name.as_ref();
        if !is_valid_table_name(table_name) {
            return Err(Error::InvalidTableName(table_name.to_owned()));
        }
        Ok(StatusStore {
            pool,
            table_name: table_name.to_owned(),
        })
    }

    pub async fn migrate(&self) -> Result<(), Error> {
        let query = format!(
            r#"
            create table if not exists {table_name}
            (
                uri text primary key,
                author_did text not null,
                status text not null,
                created_at text not null,
                indexed_at text not null
            )
            "#,
            table_name = self.table_name
        );
        sqlx::query(&query)
            .execute(&self.pool)
            .await
            .map_err(Error::MigrationFailed)?;
        Ok(())
    }

    pub async fn insert(&self, status: Status) -> Result<(), Error> {
        let query = format!(
            r#"
            insert into {table_name}
                (uri, author_did, status, created_at, indexed_at)
                values
                (?, ?, ?, ?, ?)
            on conflict(uri) do update set
                author_did = excluded.author_did,
                status = excluded.status,
                created_at = excluded.created_at,
                indexed_at = excluded.indexed_at
            "#,
            table_name = self.table_name
        );
        sqlx::query(&query)
            .bind(status.uri)
            .bind(status.author_did.as_str())
            .bind(status.status)
            .bind(status.created_at.as_str())
            .bind(status.indexed_at.as_str())
            .execute(&self.pool)
            .await
            .map_err(Error::InsertFailed)?;
        Ok(())
    }

    async fn fetch(&self, author: Option<Did>, count: usize) -> Result<Vec<Status>, Error> {
        let where_clause = author
            .map(|did| format!("where author_did = \"{}\"", did.as_str()))
            .unwrap_or(String::new());
        let query = format!(
            r#"
            select uri, author_did, status, created_at, indexed_at
            from "{table_name}"
            {where_clause}
            order by indexed_at desc
            limit {count}
            "#,
            table_name = self.table_name,
        );
        let data: Vec<Status> = sqlx::query_as(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(Error::SelectFailed)?;

        Ok(data)
    }

    pub async fn fetch_n(&self, author: Option<Did>, count: usize) -> Result<Vec<Status>, Error> {
        self.fetch(author, count).await
    }

    pub async fn fetch_one(&self, author: Option<Did>) -> Result<Option<Status>, Error> {
        let mut results = self.fetch(author, 1).await?;
        Ok(results.pop())
    }
}

fn is_valid_table_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();
    let first = chars.next().expect("expected non-empty");
    first.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// OAuthSessionStore and OAuthStateStore are very similar, so we use a macro to help
macro_rules! oauth_store {
    ($struct_name:ident, $table_name:expr, $key_ty:ty, $value_name:expr, $value_ty:ty) => {
        pub struct $struct_name {
            pool: SqlitePool,
        }

        impl $struct_name {
            pub fn new(pool: SqlitePool) -> Self {
                Self { pool }
            }

            pub async fn migrate(&self) -> Result<(), Error> {
                let query = format!(
                    r#"
                    create table if not exists {table_name}
                    (
                        key text primary key,
                        {value_name} text not null
                    )
                    "#,
                    table_name = $table_name,
                    value_name = $value_name
                );
                sqlx::query(&query)
                    .execute(&self.pool)
                    .await
                    .map_err(Error::MigrationFailed)?;
                Ok(())
            }
        }

        impl Store<$key_ty, $value_ty> for $struct_name {
            type Error = Error;

            async fn get(&self, key: &$key_ty) -> Result<Option<$value_ty>, Self::Error> {
                let query = format!(
                    r#"
                    select key, {value_name}
                    from {table_name}
                    where key = ?
                    "#,
                    value_name = $value_name,
                    table_name = $table_name
                );
                let data: Option<(String, String)> = sqlx::query_as(&query)
                    .bind(key.as_str())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(Error::SelectFailed)?;

                Ok(data
                    .map(|(_, value)| serde_json::from_str(&value).map_err(Error::Deserialization))
                    .transpose()?)
            }

            async fn set(&self, key: $key_ty, value: $value_ty) -> Result<(), Self::Error> {
                let query = format!(
                    r#"
                    insert into {table_name}
                        (key, {value_name})
                        values
                        (?, ?)
                    on conflict(key) do update set
                        {value_name} = excluded.{value_name}
                    "#,
                    table_name = $table_name,
                    value_name = $value_name
                );
                sqlx::query(&query)
                    .bind(key.as_str())
                    .bind(serde_json::to_string(&value).map_err(Error::Serialization)?)
                    .execute(&self.pool)
                    .await
                    .map_err(Error::InsertFailed)?;
                Ok(())
            }

            async fn del(&self, key: &$key_ty) -> Result<(), Self::Error> {
                let query = format!(
                    r#"
                    delete from {table_name} where key = ?
                    "#,
                    table_name = $table_name
                );
                sqlx::query(&query)
                    .bind(key.as_str())
                    .execute(&self.pool)
                    .await
                    .map_err(Error::DeleteFailed)?;
                Ok(())
            }

            async fn clear(&self) -> Result<(), Self::Error> {
                let query = format!(
                    r#"
                    delete from {table_name}
                    "#,
                    table_name = $table_name
                );
                sqlx::query(&query)
                    .execute(&self.pool)
                    .await
                    .map_err(Error::DeleteAllFailed)?;
                Ok(())
            }
        }
    };
}

oauth_store!(OAuthSessionStore, "oauth_session", Did, "session", Session);
impl SessionStore for OAuthSessionStore {}

oauth_store!(
    OAuthStateStore,
    "oauth_state",
    String,
    "state",
    InternalStateData
);
impl StateStore for OAuthStateStore {}
