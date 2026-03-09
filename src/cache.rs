use crate::config::CacheConfig;
use crate::errors::ProxyError;
use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

const CACHE_DIR: &str = ".cache";
const OBJECTS_DIR: &str = "objects";
const DB_FILE: &str = "cache.sqlite";

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub key: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_path: Option<String>,
    pub body_size: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_access: i64,
}

#[derive(Debug, Clone)]
pub struct CacheStoreRequest {
    pub key: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_path: Option<String>,
    pub body_size: u64,
    pub created_at: i64,
    pub expires_at: i64,
}

#[async_trait]
pub trait Cache: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_enabled(&self) -> bool;
    async fn get(&self, method: &str, url: &str) -> Result<Option<CacheEntry>, ProxyError>;
    async fn store(&self, entry: CacheStoreRequest) -> Result<(), ProxyError>;
    async fn delete(&self, key: &str) -> Result<(), ProxyError>;
    fn cache_dir(&self) -> &Path;
}

#[derive(Debug)]
pub struct NoopCache;

#[async_trait]
impl Cache for NoopCache {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn is_enabled(&self) -> bool {
        false
    }

    async fn get(&self, _method: &str, _url: &str) -> Result<Option<CacheEntry>, ProxyError> {
        Ok(None)
    }

    async fn store(&self, _entry: CacheStoreRequest) -> Result<(), ProxyError> {
        Ok(())
    }

    async fn delete(&self, _key: &str) -> Result<(), ProxyError> {
        Ok(())
    }

    fn cache_dir(&self) -> &Path {
        Path::new(".")
    }
}

#[derive(Debug)]
pub struct SqliteCache {
    dir: PathBuf,
    db_path: PathBuf,
    max_entries: usize,
}

impl SqliteCache {
    pub async fn new(config: &CacheConfig) -> Result<Self, ProxyError> {
        let dir = PathBuf::from(CACHE_DIR);
        let objects_dir = dir.join(OBJECTS_DIR);
        fs::create_dir_all(&objects_dir).await?;

        let db_path = dir.join(DB_FILE);
        init_db(&db_path)?;

        Ok(Self {
            dir,
            db_path,
            max_entries: config.max_entries,
        })
    }

    fn now_unix() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn cache_key(method: &str, url: &str) -> String {
        format!("{method} {url}")
    }
}

#[async_trait]
impl Cache for SqliteCache {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn get(&self, method: &str, url: &str) -> Result<Option<CacheEntry>, ProxyError> {
        let key = Self::cache_key(method, url);
        let db_path = self.db_path.clone();
        let now = Self::now_unix();
        let entry = tokio::task::spawn_blocking(move || {
            let conn = Connection::open(db_path)?;
            let mut stmt = conn.prepare(
                "SELECT key, method, url, status, headers, body_path, body_size, created_at, expires_at, last_access \
                 FROM cache_entries WHERE key = ?1",
            )?;
            let row = stmt
                .query_row([&key], |row| {
                    let headers: String = row.get(4)?;
                    Ok(CacheEntry {
                        key: row.get(0)?,
                        method: row.get(1)?,
                        url: row.get(2)?,
                        status: row.get::<_, u16>(3)?,
                        headers: serde_json::from_str(&headers).unwrap_or_default(),
                        body_path: row.get::<_, Option<String>>(5)?,
                        body_size: row.get::<_, i64>(6)? as u64,
                        created_at: row.get(7)?,
                        expires_at: row.get(8)?,
                        last_access: row.get(9)?,
                    })
                })
                .optional()?;
            Ok::<_, rusqlite::Error>(row)
        })
        .await
        .map_err(|_| ProxyError::Internal)??;

        let Some(mut entry) = entry else {
            return Ok(None);
        };

        if entry.expires_at <= now {
            self.delete(&entry.key).await?;
            return Ok(None);
        }

        if let Some(path) = &entry.body_path {
            if !Path::new(path).exists() {
                self.delete(&entry.key).await?;
                return Ok(None);
            }
        }

        let db_path = self.db_path.clone();
        let key_clone = entry.key.clone();
        entry.last_access = now;
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(db_path)?;
            conn.execute(
                "UPDATE cache_entries SET last_access = ?1 WHERE key = ?2",
                params![now, key_clone],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|_| ProxyError::Internal)??;

        Ok(Some(entry))
    }

    async fn store(&self, entry: CacheStoreRequest) -> Result<(), ProxyError> {
        let db_path = self.db_path.clone();
        let max_entries = self.max_entries;
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(db_path.clone())?;
            let headers = serde_json::to_string(&entry.headers).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "INSERT INTO cache_entries \
                 (key, method, url, status, headers, body_path, body_size, created_at, expires_at, last_access) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(key) DO UPDATE SET \
                   method=excluded.method, \
                   url=excluded.url, \
                   status=excluded.status, \
                   headers=excluded.headers, \
                   body_path=excluded.body_path, \
                   body_size=excluded.body_size, \
                   created_at=excluded.created_at, \
                   expires_at=excluded.expires_at, \
                   last_access=excluded.last_access",
                params![
                    entry.key,
                    entry.method,
                    entry.url,
                    entry.status,
                    headers,
                    entry.body_path,
                    entry.body_size as i64,
                    entry.created_at,
                    entry.expires_at,
                    entry.created_at,
                ],
            )?;

            if max_entries > 0 {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM cache_entries",
                    [],
                    |row| row.get(0),
                )?;
                if count as usize > max_entries {
                    let overflow = count as usize - max_entries;
                    let mut stmt = conn.prepare(
                        "SELECT key, body_path FROM cache_entries ORDER BY last_access ASC LIMIT ?1",
                    )?;
                    let mut rows = stmt.query([overflow as i64])?;
                    let mut keys = Vec::new();
                    while let Some(row) = rows.next()? {
                        let key: String = row.get(0)?;
                        let body_path: Option<String> = row.get(1)?;
                        keys.push((key, body_path));
                    }
                    for (key, body_path) in keys {
                        conn.execute("DELETE FROM cache_entries WHERE key = ?1", [key])?;
                        if let Some(path) = body_path {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }

            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|_| ProxyError::Internal)??;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), ProxyError> {
        let db_path = self.db_path.clone();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(db_path)?;
            let body_path: Option<String> = conn
                .query_row(
                    "SELECT body_path FROM cache_entries WHERE key = ?1",
                    [&key],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            conn.execute("DELETE FROM cache_entries WHERE key = ?1", [&key])?;
            if let Some(path) = body_path {
                let _ = std::fs::remove_file(path);
            }
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|_| ProxyError::Internal)??;

        Ok(())
    }

    fn cache_dir(&self) -> &Path {
        &self.dir
    }
}

pub async fn build_cache(config: &CacheConfig) -> Result<std::sync::Arc<dyn Cache>, ProxyError> {
    if !config.enabled {
        return Ok(std::sync::Arc::new(NoopCache));
    }

    let cache = SqliteCache::new(config).await?;
    Ok(std::sync::Arc::new(cache))
}

pub fn cache_key(method: &str, url: &str) -> String {
    SqliteCache::cache_key(method, url)
}

pub fn now_unix() -> i64 {
    SqliteCache::now_unix()
}

pub fn cache_objects_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join(OBJECTS_DIR)
}

fn init_db(db_path: &Path) -> Result<(), ProxyError> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache_entries (\
            key TEXT PRIMARY KEY,\
            method TEXT NOT NULL,\
            url TEXT NOT NULL,\
            status INTEGER NOT NULL,\
            headers TEXT NOT NULL,\
            body_path TEXT,\
            body_size INTEGER NOT NULL,\
            created_at INTEGER NOT NULL,\
            expires_at INTEGER NOT NULL,\
            last_access INTEGER NOT NULL\
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cache_lru ON cache_entries(last_access)",
        [],
    )?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheControl {
    pub no_store: bool,
    pub no_cache: bool,
    pub max_age: Option<u64>,
}

pub fn parse_cache_control(value: &str) -> CacheControl {
    let mut control = CacheControl {
        no_store: false,
        no_cache: false,
        max_age: None,
    };

    for part in value.split(',').map(|s| s.trim()) {
        if part.eq_ignore_ascii_case("no-store") {
            control.no_store = true;
        } else if part.eq_ignore_ascii_case("no-cache") {
            control.no_cache = true;
        } else if let Some(rest) = part.strip_prefix("max-age=") {
            if let Ok(age) = rest.parse::<u64>() {
                control.max_age = Some(age);
            }
        }
    }

    control
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;
    use serial_test::serial;

    fn clear_cache_dir() {
        let _ = std::fs::remove_dir_all(".cache");
    }

    #[test]
    fn cache_control_parsing() {
        let control = parse_cache_control("max-age=60, no-cache");
        assert_eq!(control.max_age, Some(60));
        assert!(control.no_cache);
        assert!(!control.no_store);
    }

    #[tokio::test]
    #[serial]
    async fn sqlite_cache_round_trips_metadata_and_deletes_body_file() {
        clear_cache_dir();

        let config = CacheConfig {
            enabled: true,
            ..CacheConfig::default()
        };
        let cache = SqliteCache::new(&config).await.unwrap();
        let body_path = cache_objects_dir(cache.cache_dir()).join("roundtrip-body");
        tokio::fs::write(&body_path, b"hello-cache").await.unwrap();

        let entry = CacheStoreRequest {
            key: "GET http://example.com/asset".into(),
            method: "GET".into(),
            url: "http://example.com/asset".into(),
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body_path: Some(body_path.to_string_lossy().to_string()),
            body_size: 11,
            created_at: now_unix(),
            expires_at: now_unix() + 60,
        };

        cache.store(entry).await.unwrap();
        let loaded = cache
            .get("GET", "http://example.com/asset")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, 200);
        assert_eq!(loaded.body_size, 11);
        assert_eq!(loaded.headers.len(), 1);

        cache.delete(&loaded.key).await.unwrap();
        assert!(cache.get("GET", "http://example.com/asset").await.unwrap().is_none());
        assert!(!body_path.exists());
    }

    #[tokio::test]
    #[serial]
    async fn sqlite_cache_removes_expired_entries_on_lookup() {
        clear_cache_dir();

        let config = CacheConfig {
            enabled: true,
            ..CacheConfig::default()
        };
        let cache = SqliteCache::new(&config).await.unwrap();

        cache
            .store(CacheStoreRequest {
                key: "GET http://example.com/expired".into(),
                method: "GET".into(),
                url: "http://example.com/expired".into(),
                status: 200,
                headers: vec![],
                body_path: None,
                body_size: 0,
                created_at: now_unix() - 10,
                expires_at: now_unix() - 1,
            })
            .await
            .unwrap();

        assert!(cache.get("GET", "http://example.com/expired").await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial]
    async fn sqlite_cache_eviction_removes_oldest_entry_and_its_file() {
        clear_cache_dir();

        let config = CacheConfig {
            enabled: true,
            max_entries: 1,
            ..CacheConfig::default()
        };
        let cache = SqliteCache::new(&config).await.unwrap();
        let objects_dir = cache_objects_dir(cache.cache_dir());
        let old_body = objects_dir.join("old-body");
        let new_body = objects_dir.join("new-body");
        tokio::fs::write(&old_body, b"old").await.unwrap();
        tokio::fs::write(&new_body, b"new").await.unwrap();

        cache
            .store(CacheStoreRequest {
                key: "GET http://example.com/old".into(),
                method: "GET".into(),
                url: "http://example.com/old".into(),
                status: 200,
                headers: vec![],
                body_path: Some(old_body.to_string_lossy().to_string()),
                body_size: 3,
                created_at: now_unix() - 5,
                expires_at: now_unix() + 60,
            })
            .await
            .unwrap();
        cache
            .store(CacheStoreRequest {
                key: "GET http://example.com/new".into(),
                method: "GET".into(),
                url: "http://example.com/new".into(),
                status: 200,
                headers: vec![],
                body_path: Some(new_body.to_string_lossy().to_string()),
                body_size: 3,
                created_at: now_unix(),
                expires_at: now_unix() + 60,
            })
            .await
            .unwrap();

        assert!(cache.get("GET", "http://example.com/old").await.unwrap().is_none());
        assert!(cache.get("GET", "http://example.com/new").await.unwrap().is_some());
        assert!(!old_body.exists());
        assert!(new_body.exists());
    }

    #[tokio::test]
    #[serial]
    async fn sqlite_cache_invalidates_dangling_body_path() {
        clear_cache_dir();

        let config = CacheConfig {
            enabled: true,
            ..CacheConfig::default()
        };
        let cache = SqliteCache::new(&config).await.unwrap();
        let missing_body = cache_objects_dir(cache.cache_dir()).join("missing-body");

        cache
            .store(CacheStoreRequest {
                key: "GET http://example.com/missing".into(),
                method: "GET".into(),
                url: "http://example.com/missing".into(),
                status: 200,
                headers: vec![],
                body_path: Some(missing_body.to_string_lossy().to_string()),
                body_size: 10,
                created_at: now_unix(),
                expires_at: now_unix() + 60,
            })
            .await
            .unwrap();

        assert!(cache.get("GET", "http://example.com/missing").await.unwrap().is_none());
    }
}
