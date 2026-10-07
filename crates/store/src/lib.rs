//! SQLite persistence for `SwiftFetch`.
//!
//! The database is the single source of truth for downloads, the segment
//! journal, queues, categories, settings and site logins (passwords live in
//! the OS keyring and are only referenced by handle). The database opens in
//! WAL mode so readers never block the writer and every commit is crash-safe;
//! schema changes ship as [refinery] migrations embedded in this crate.
//!
//! Single-writer discipline: the GUI process owns the write connection. WAL
//! lets concurrent readers proceed — see docs/system-design.md §5.
//!
//! [refinery]: https://docs.rs/refinery

pub mod repos;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use swiftfetch_common::APP_NAME;

/// Embedded schema migrations, applied on open (refinery).
#[allow(missing_docs)] // refinery's generated runner items carry no doc comments
mod embedded {
    use rusqlite::Connection;

    refinery::embed_migrations!("migrations");

    /// Runs all pending embedded migrations against `conn`.
    ///
    /// # Errors
    ///
    /// Returns [`refinery::Error`] if any migration fails.
    pub fn run(conn: &mut Connection) -> Result<refinery::Report, refinery::Error> {
        migrations::runner().run(conn)
    }
}

/// Errors returned by store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database file or its parent directory could not be created/opened.
    #[error("could not open database `{path}`: {message}")]
    Open {
        /// Database path that failed to open.
        path: PathBuf,
        /// Human-readable reason for the failure.
        message: String,
    },
    /// A schema migration failed.
    #[error(transparent)]
    Migrate(#[from] refinery::Error),
    /// A SQLite statement failed.
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

/// Handle to the `SwiftFetch` SQLite database (WAL mode, migrations applied).
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Opens (creating if necessary) the database at `path`, enables WAL and
    /// runs any pending migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] when the file or its parent directory
    /// cannot be created or opened, [`StoreError::Migrate`] when a migration
    /// fails, and [`StoreError::Sql`] when a pragma fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| StoreError::Open {
                path: path.to_path_buf(),
                message: err.to_string(),
            })?;
        }
        let conn = Connection::open(path).map_err(|err| StoreError::Open {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Self::init(conn, path.to_path_buf())
    }

    /// Opens the database at the OS app-data directory (`%APPDATA%\SwiftFetch`
    /// on Windows, `~/Library/Application Support/SwiftFetch` on macOS,
    /// `$XDG_DATA_HOME/SwiftFetch` on Linux).
    ///
    /// # Errors
    ///
    /// Same as [`Store::open`], plus [`StoreError::Open`] when the OS data
    /// directory cannot be resolved.
    pub fn open_default() -> Result<Self, StoreError> {
        let dir = default_data_dir().ok_or_else(|| StoreError::Open {
            path: PathBuf::from("<app-data>"),
            message: "could not resolve the OS app-data directory".into(),
        })?;
        Self::open(&dir.join("swiftfetch.db"))
    }

    /// The database file path this store was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The effective journal mode as reported by SQLite (expected: `wal`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] if the pragma query fails.
    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let mode = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode)
    }

    /// Names of all tables in the schema (including migration bookkeeping).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] if the catalog query fails.
    pub fn table_names(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    /// Runs `f` with the underlying connection.
    ///
    /// Escape hatch used by repositories and tests; keep transactions short
    /// and never hold the connection across await points (the connection is
    /// not `Sync`).
    ///
    /// # Errors
    ///
    /// Returns whatever [`rusqlite::Error`] `f` produces.
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let value = f(&self.conn)?;
        Ok(value)
    }

    /// Runs `f` inside a `BEGIN IMMEDIATE` transaction: the write lock is
    /// taken up front, so cross-process writers (the CLI) serialize cleanly
    /// with the GUI's writes while WAL keeps readers proceeding.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] when the transaction or `f` fails; the
    /// transaction is rolled back on error.
    pub fn with_conn_immediate<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(&self.conn) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(err) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(err.into())
            }
        }
    }

    fn init(conn: Connection, path: PathBuf) -> Result<Self, StoreError> {
        let applied = conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        })?;
        tracing::debug!(%applied, "journal mode set");
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut conn = conn;
        let report = embedded::run(&mut conn)?;
        let applied_count = report.applied_migrations().len();
        if applied_count > 0 {
            tracing::info!(applied_count, "schema migrations applied");
        }
        Ok(Self { conn, path })
    }
}

/// Default per-user data directory for the `SwiftFetch` database
/// (`%APPDATA%\SwiftFetch` on Windows, `~/Library/Application
/// Support/SwiftFetch` on macOS, `$XDG_DATA_HOME/SwiftFetch` on Linux).
#[must_use]
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|base| base.join(APP_NAME))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn default_data_dir_resolves() {
        let dir = default_data_dir();
        assert!(dir.is_some(), "OS data directory must resolve");
        let dir = dir.expect("checked above");
        assert!(dir.ends_with(APP_NAME));
    }
}
