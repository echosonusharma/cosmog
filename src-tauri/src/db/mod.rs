//! SQLite-backed application state: [`Db`] wraps a [`tokio_rusqlite::Connection`]
//! with domain methods in submodules. Schema evolves via ordered migrations in
//! [`MIGRATIONS`], tracked in `schema_migrations`. MIGRATIONS is append-only.

pub mod accounts;
pub mod cache;
pub mod capabilities;
pub mod encryption;
pub mod night_watcher;
pub mod request_logs;
pub mod settings;
pub mod transfers;

use std::path::Path;

use tokio_rusqlite::Connection;

use crate::error::AppResult;

/// Handle to the application database; cheap to clone (the connection is
/// `Arc`-shared by `tokio-rusqlite`).
#[derive(Clone)]
pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub async fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).await?;
        let db = Db { conn };
        db.apply_pragmas().await?;
        db.run_migrations().await?;
        Ok(db)
    }

    async fn apply_pragmas(&self) -> AppResult<()> {
        self.conn
            .call(|conn| {
                conn.execute_batch(
                    "PRAGMA journal_mode = WAL; \
                     PRAGMA synchronous = NORMAL; \
                     PRAGMA foreign_keys = ON; \
                     PRAGMA busy_timeout = 5000; \
                     PRAGMA cache_size = -8000; \
                     PRAGMA temp_store = MEMORY;",
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    async fn run_migrations(&self) -> AppResult<()> {
        self.conn
            .call(|conn| {
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS schema_migrations (
                        version INTEGER PRIMARY KEY,
                        applied_at INTEGER NOT NULL
                    )",
                    [],
                )?;

                let current: i64 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                for migration in MIGRATIONS {
                    if migration.version as i64 <= current {
                        continue;
                    }
                    let tx = conn.transaction()?;
                    tx.execute_batch(migration.sql)?;
                    tx.execute(
                        "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                        rusqlite::params![
                            migration.version,
                            chrono::Utc::now().timestamp()
                        ],
                    )?;
                    tx.commit()?;
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }
}

impl Db {
    /// Atomic copy via SQLite's Backup API — safe against concurrent writers
    /// (coordinates with the WAL), unlike a raw `fs::copy`.
    pub async fn backup_to(&self, dest: std::path::PathBuf) -> AppResult<()> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        self.conn
            .call(move |conn| {
                let mut dest_conn = rusqlite::Connection::open(&dest)?;
                let backup = rusqlite::backup::Backup::new(conn, &mut dest_conn)?;
                // Large steps + small sleep: a tiny step count with zero sleep
                // busy-spins and holds a long read snapshot against the WAL.
                backup.run_to_completion(4096, std::time::Duration::from_millis(5), None)?;
                drop(backup);
                // Explicit close so a failed flush surfaces instead of being dropped.
                if let Err((_, err)) = dest_conn.close() {
                    return Err(tokio_rusqlite::Error::from(err));
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }
}

/// One forward-only schema change, identified by a monotonically increasing version.
struct Migration {
    version: u32,
    sql: &'static str,
}

/// Ordered, append-only list of schema migrations. **Never** edit or reorder.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: r#"
            CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                protocol TEXT NOT NULL,
                endpoint TEXT,
                region TEXT NOT NULL,
                access_key_id TEXT NOT NULL,
                addressing_style TEXT NOT NULL DEFAULT 'auto',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_accounts_protocol ON accounts(protocol);
        "#,
    },
    Migration {
        version: 2,
        sql: r#"
            CREATE TABLE IF NOT EXISTS transfers (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                bucket TEXT NOT NULL,
                key TEXT NOT NULL,
                direction TEXT NOT NULL,
                local_path TEXT NOT NULL,
                bytes_total INTEGER,
                bytes_done INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                upload_id TEXT,
                parts_json TEXT,
                error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_transfers_status ON transfers(status);
            CREATE INDEX IF NOT EXISTS idx_transfers_account ON transfers(account_id);
        "#,
    },
    Migration {
        version: 3,
        sql: r#"
            CREATE TABLE IF NOT EXISTS bucket_index (
                account_id TEXT NOT NULL,
                bucket TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0,
                last_full_sync_at INTEGER,
                object_count INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (account_id, bucket),
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS cached_objects (
                account_id TEXT NOT NULL,
                bucket TEXT NOT NULL,
                key TEXT NOT NULL,
                size INTEGER NOT NULL DEFAULT 0,
                etag TEXT,
                last_modified INTEGER,
                storage_class TEXT,
                content_type TEXT,
                extension TEXT,
                basename TEXT NOT NULL DEFAULT '',
                parent_prefix TEXT NOT NULL DEFAULT '',
                version_id TEXT,
                seen INTEGER NOT NULL DEFAULT 1,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (account_id, bucket, key),
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_co_prefix
                ON cached_objects(account_id, bucket, parent_prefix);
            CREATE INDEX IF NOT EXISTS idx_co_ext
                ON cached_objects(account_id, bucket, extension);
            CREATE INDEX IF NOT EXISTS idx_co_modified
                ON cached_objects(account_id, bucket, last_modified);
            CREATE INDEX IF NOT EXISTS idx_co_size
                ON cached_objects(account_id, bucket, size);
            CREATE INDEX IF NOT EXISTS idx_co_storage
                ON cached_objects(account_id, bucket, storage_class);
            CREATE INDEX IF NOT EXISTS idx_co_ctype
                ON cached_objects(account_id, bucket, content_type);

            CREATE VIRTUAL TABLE IF NOT EXISTS cached_objects_fts USING fts5(
                key,
                basename,
                content='cached_objects',
                content_rowid='rowid',
                tokenize="unicode61 separators '/_.-'"
            );

            CREATE TRIGGER IF NOT EXISTS cached_objects_ai
            AFTER INSERT ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_ad
            AFTER DELETE ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_au
            AFTER UPDATE ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;

            CREATE TABLE IF NOT EXISTS prefix_sync (
                account_id TEXT NOT NULL,
                bucket TEXT NOT NULL,
                prefix TEXT NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (account_id, bucket, prefix),
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 4,
        sql: r#"
            ALTER TABLE bucket_index ADD COLUMN scan_continuation TEXT;
            ALTER TABLE bucket_index ADD COLUMN scan_started_at INTEGER;
        "#,
    },
    Migration {
        version: 5,
        sql: r#"
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
        "#,
    },
    Migration {
        version: 6,
        sql: r#"
            CREATE TABLE IF NOT EXISTS account_capabilities (
                account_id TEXT PRIMARY KEY,
                list_buckets INTEGER,
                create_bucket INTEGER,
                probed_at INTEGER NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS bucket_capabilities (
                account_id TEXT NOT NULL,
                bucket TEXT NOT NULL,
                head_bucket INTEGER,
                list_objects INTEGER,
                get_versioning INTEGER,
                get_location INTEGER,
                last_put_result TEXT,
                last_put_at INTEGER,
                last_delete_result TEXT,
                last_delete_at INTEGER,
                probed_at INTEGER,
                PRIMARY KEY (account_id, bucket),
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 7,
        sql: r#"
            ALTER TABLE bucket_index ADD COLUMN auto_reindex_secs INTEGER;
        "#,
    },
    Migration {
        version: 8,
        sql: r#"
            ALTER TABLE transfers ADD COLUMN options_json TEXT;
        "#,
    },
    Migration {
        version: 9,
        sql: r#"
            DROP INDEX IF EXISTS idx_co_prefix;
            ALTER TABLE cached_objects DROP COLUMN parent_prefix;
        "#,
    },
    Migration {
        version: 10,
        sql: r#"
            CREATE TABLE IF NOT EXISTS request_logs (
                id TEXT PRIMARY KEY,
                account_id TEXT,
                account_name TEXT,
                operation TEXT NOT NULL,
                bucket TEXT,
                key TEXT,
                status TEXT NOT NULL,
                error_code TEXT,
                error_msg TEXT,
                duration_ms INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rl_created ON request_logs(created_at);
            CREATE INDEX IF NOT EXISTS idx_rl_account ON request_logs(account_id);
            CREATE INDEX IF NOT EXISTS idx_rl_status ON request_logs(status);
        "#,
    },
    Migration {
        version: 11,
        sql: r#"
            ALTER TABLE request_logs ADD COLUMN http_method TEXT;
            ALTER TABLE request_logs ADD COLUMN request_url TEXT;
            ALTER TABLE request_logs ADD COLUMN request_params TEXT;
            ALTER TABLE request_logs ADD COLUMN response_status INTEGER;
        "#,
    },
    Migration {
        version: 12,
        sql: r#"
            ALTER TABLE request_logs ADD COLUMN response_meta TEXT;
        "#,
    },
    Migration {
        version: 13,
        sql: r#"
            DROP TRIGGER IF EXISTS cached_objects_ai;
            DROP TRIGGER IF EXISTS cached_objects_ad;
            DROP TRIGGER IF EXISTS cached_objects_au;
            DROP TABLE IF EXISTS cached_objects_fts;

            CREATE VIRTUAL TABLE IF NOT EXISTS cached_objects_fts USING fts5(
                key,
                basename,
                content='cached_objects',
                content_rowid='rowid',
                tokenize='trigram'
            );

            CREATE TRIGGER IF NOT EXISTS cached_objects_ai
            AFTER INSERT ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_ad
            AFTER DELETE ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_au
            AFTER UPDATE ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;

            INSERT INTO cached_objects_fts(cached_objects_fts) VALUES('rebuild');
        "#,
    },
    Migration {
        version: 14,
        sql: r#"
            CREATE TABLE IF NOT EXISTS bucket_encryption (
                account_id TEXT NOT NULL,
                bucket     TEXT NOT NULL,
                salt_hex   TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (account_id, bucket),
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 15,
        sql: r#"
            CREATE TABLE IF NOT EXISTS nw_watch (
                id            TEXT PRIMARY KEY,
                account_id    TEXT NOT NULL,
                bucket        TEXT NOT NULL,
                local_dir     TEXT NOT NULL,
                key_prefix    TEXT NOT NULL DEFAULT '',
                ignore_file   TEXT,
                delete_policy TEXT NOT NULL DEFAULT 'keep',
                full_scan_secs INTEGER NOT NULL DEFAULT 300,
                enabled       INTEGER NOT NULL DEFAULT 1,
                last_scan_at  INTEGER,
                last_error    TEXT,
                created_at    INTEGER NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS nw_file_state (
                watch_id    TEXT NOT NULL,
                rel_path    TEXT NOT NULL,
                hash        TEXT NOT NULL,
                mtime       INTEGER NOT NULL,
                size        INTEGER NOT NULL,
                synced_etag TEXT,
                synced_at   INTEGER NOT NULL,
                PRIMARY KEY (watch_id, rel_path),
                FOREIGN KEY(watch_id) REFERENCES nw_watch(id) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 16,
        // Android: a watched location is a SAF tree content:// URI; NULL on desktop.
        sql: r#"
            ALTER TABLE nw_watch ADD COLUMN tree_uri TEXT;
        "#,
    },
    Migration {
        version: 17,
        // Who started the transfer; 'nightwatch' rows are silent background syncs.
        sql: r#"
            ALTER TABLE transfers ADD COLUMN origin TEXT NOT NULL DEFAULT 'user';
        "#,
    },
    Migration {
        version: 18,
        // Per-file upload-retry backoff: after MAX consecutive failures the
        // file pauses until retry_after so a broken file stops re-enqueuing.
        sql: r#"
            CREATE TABLE IF NOT EXISTS nw_file_retry (
                watch_id    TEXT NOT NULL,
                rel_path    TEXT NOT NULL,
                fail_count  INTEGER NOT NULL,
                retry_after INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (watch_id, rel_path),
                FOREIGN KEY(watch_id) REFERENCES nw_watch(id) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 19,
        // Recreate FTS triggers with a WHEN guard (`IS NOT`, NULL-safe):
        // otherwise every-column updates re-tokenize the trigram index.
        sql: r#"
            DROP TRIGGER IF EXISTS cached_objects_ai;
            DROP TRIGGER IF EXISTS cached_objects_ad;
            DROP TRIGGER IF EXISTS cached_objects_au;

            CREATE TRIGGER IF NOT EXISTS cached_objects_ai
            AFTER INSERT ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_ad
            AFTER DELETE ON cached_objects BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
            END;

            CREATE TRIGGER IF NOT EXISTS cached_objects_au
            AFTER UPDATE ON cached_objects
            WHEN new.key IS NOT old.key OR new.basename IS NOT old.basename
            BEGIN
                INSERT INTO cached_objects_fts(cached_objects_fts, rowid, key, basename)
                VALUES('delete', old.rowid, old.key, old.basename);
                INSERT INTO cached_objects_fts(rowid, key, basename)
                VALUES (new.rowid, new.key, new.basename);
            END;
        "#,
    },
];
