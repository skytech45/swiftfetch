//! Typed repositories over the SQLite schema: downloads, segments, queues,
//! categories and settings. All functions take a [`Store`] and run
//! synchronously — callers on async contexts wrap in `spawn_blocking`.

use std::path::PathBuf;

use rusqlite::OptionalExtension;

use crate::Store;
use crate::StoreError;

/// A `downloads` row for UI listing.
#[derive(Debug, Clone)]
pub struct DownloadRow {
    /// Job id (uuid v4).
    pub id: String,
    /// Current URL.
    pub url: String,
    /// Final destination path.
    pub final_path: String,
    /// Total length when known.
    pub total_len: Option<i64>,
    /// Denormalized done-bytes sum.
    pub done_bytes: i64,
    /// Job state string.
    pub state: String,
    /// Category id, if assigned.
    pub category_id: Option<String>,
    /// Queue id, if enqueued.
    pub queue_id: Option<String>,
    /// Error code, when errored.
    pub error_code: Option<String>,
    /// Error message, when errored.
    pub error_msg: Option<String>,
    /// Max connections for this job.
    pub max_conns: i64,
    /// Creation timestamp.
    pub created_at: String,
    /// Filename (derived from `final_path`).
    pub filename: String,
}

/// A `categories` row.
#[derive(Debug, Clone)]
pub struct CategoryRow {
    /// Category id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// JSON array of extensions, e.g. `["mp4","mkv"]`.
    pub extensions: String,
    /// Absolute default folder.
    pub folder: String,
}

/// A `queues` row.
#[derive(Debug, Clone)]
pub struct QueueRow {
    /// Queue id.
    pub id: String,
    /// Queue name (unique).
    pub name: String,
    /// Max concurrent downloads from this queue.
    pub max_concurrent: i64,
    /// Whether the queue is active (auto-starts items).
    pub is_active: i64,
}

impl DownloadRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let final_path: String = r.get(2)?;
        let filename = std::path::Path::new(&final_path)
            .file_name()
            .map_or_else(|| final_path.clone(), |n| n.to_string_lossy().into_owned());
        Ok(Self {
            id: r.get(0)?,
            url: r.get(1)?,
            filename,
            final_path,
            total_len: r.get(3)?,
            done_bytes: r.get(4)?,
            state: r.get(5)?,
            category_id: r.get(6)?,
            queue_id: r.get(7)?,
            error_code: r.get(8)?,
            error_msg: r.get(9)?,
            max_conns: r.get(10)?,
            created_at: r.get(11)?,
        })
    }
}

const DOWNLOAD_COLS: &str = "id, url, final_path, total_len, done_bytes, state, category_id, \
     queue_id, error_code, error_msg, max_conns, created_at";

/// Insert a minimal downloads row (the engine's journal fills the rest).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn insert_download(
    store: &Store,
    id: &str,
    url: &str,
    final_path: &str,
    category_id: Option<&str>,
    queue_id: Option<&str>,
    max_conns: i64,
) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO downloads (id, url, final_path, part_path, category_id, \
             queue_id, max_conns, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3 || '.sfpart', ?4, ?5, ?6, \
             strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            rusqlite::params![id, url, final_path, category_id, queue_id, max_conns],
        )?;
        Ok(())
    })
}

/// Lists downloads, newest first.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn list_downloads(store: &Store) -> Result<Vec<DownloadRow>, StoreError> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {DOWNLOAD_COLS} FROM downloads ORDER BY created_at DESC, id"
        ))?;
        let rows = stmt
            .query_map([], DownloadRow::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// Loads one download row.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn get_download(store: &Store, id: &str) -> Result<Option<DownloadRow>, StoreError> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {DOWNLOAD_COLS} FROM downloads WHERE id = ?1"
        ))?;
        let row = stmt.query_row([id], DownloadRow::from_row).optional()?;
        Ok(row)
    })
}

/// Deletes a download row (segments cascade).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn delete_download(store: &Store, id: &str) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    })
}

/// Sets a download's category.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn set_category(store: &Store, id: &str, category_id: Option<&str>) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE downloads SET category_id = ?2, updated_at = \
             strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
            rusqlite::params![id, category_id],
        )?;
        Ok(())
    })
}

/// One journaled segment: (idx, start, end, done, state).
pub type SegmentTuple = (i64, i64, i64, i64, String);

/// Loads all segments of a job ordered by index (progress view).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn list_segments(store: &Store, job_id: &str) -> Result<Vec<SegmentTuple>, StoreError> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT idx, start_o, end_o, done, state FROM segments WHERE job_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt
            .query_map([job_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

// ── Categories ───────────────────────────────────────────────────────────

/// Seeds the default categories if absent.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn seed_default_categories(
    store: &Store,
    base_dir: &std::path::Path,
) -> Result<(), StoreError> {
    let defaults: [(&str, &str, &str); 6] = [
        (
            "compressed",
            "Compressed",
            "[\"zip\",\"rar\",\"7z\",\"tar\",\"gz\"]",
        ),
        (
            "documents",
            "Documents",
            "[\"pdf\",\"doc\",\"docx\",\"txt\",\"md\",\"epub\"]",
        ),
        (
            "music",
            "Music",
            "[\"mp3\",\"flac\",\"wav\",\"m4a\",\"ogg\"]",
        ),
        (
            "programs",
            "Programs",
            "[\"exe\",\"msi\",\"apk\",\"dmg\",\"deb\",\"appimage\"]",
        ),
        (
            "video",
            "Video",
            "[\"mp4\",\"mkv\",\"webm\",\"avi\",\"mov\",\"ts\"]",
        ),
        (
            "images",
            "Images",
            "[\"jpg\",\"png\",\"gif\",\"webp\",\"svg\"]",
        ),
    ];
    store.with_conn(|conn| {
        for (id, name, exts) in defaults {
            conn.execute(
                "INSERT OR IGNORE INTO categories (id, name, extensions, folder) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, name, exts, base_dir.join(name).display().to_string()],
            )?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO categories (id, name, extensions, folder) \
             VALUES ('other', 'Other', '[]', ?1)",
            rusqlite::params![base_dir.join("Other").display().to_string()],
        )?;
        Ok(())
    })
}

/// Lists all categories ordered by id.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn list_categories(store: &Store) -> Result<Vec<CategoryRow>, StoreError> {
    store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, name, extensions, folder FROM categories ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CategoryRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    extensions: r.get(2)?,
                    folder: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// Creates a custom category.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn create_category(
    store: &Store,
    id: &str,
    name: &str,
    extensions: &str,
    folder: &str,
) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO categories (id, name, extensions, folder) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, name, extensions, folder],
        )?;
        Ok(())
    })
}

/// Deletes a category; returns the number of rows removed (0 for unknown).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn delete_category(store: &Store, id: &str) -> Result<usize, StoreError> {
    store.with_conn(|conn| {
        let n = conn.execute("DELETE FROM categories WHERE id = ?1", [id])?;
        Ok(n)
    })
}

/// Assigns a category by file extension: returns the matching category id,
/// or `other` as the fallback. The extension map is data (the `categories`
/// table), not code.
#[must_use]
pub fn categorize(categories: &[CategoryRow], filename: &str) -> Option<String> {
    let ext = filename.rsplit('.').next()?.to_ascii_lowercase();
    for cat in categories {
        let Ok(list) = serde_json::from_str::<Vec<String>>(&cat.extensions) else {
            continue;
        };
        if list.iter().any(|e| e.to_ascii_lowercase() == ext) {
            return Some(cat.id.clone());
        }
    }
    categories
        .iter()
        .find(|c| c.id == "other")
        .map(|c| c.id.clone())
}

// ── Queues ───────────────────────────────────────────────────────────────

/// Creates a named queue.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn create_queue(
    store: &Store,
    id: &str,
    name: &str,
    max_concurrent: i64,
) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO queues (id, name, max_concurrent, created_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            rusqlite::params![id, name, max_concurrent],
        )?;
        Ok(())
    })
}

/// Lists queues ordered by name.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn list_queues(store: &Store) -> Result<Vec<QueueRow>, StoreError> {
    store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, name, max_concurrent, is_active FROM queues ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(QueueRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    max_concurrent: r.get(2)?,
                    is_active: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// Deletes a queue (memberships cascade; downloads are kept).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn delete_queue(store: &Store, id: &str) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM queues WHERE id = ?1", [id])?;
        Ok(())
    })
}

/// Appends a download to a queue at the end.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn enqueue(store: &Store, queue_id: &str, job_id: &str) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM queue_items WHERE queue_id = ?1",
                [queue_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        conn.execute(
            "INSERT OR IGNORE INTO queue_items (queue_id, job_id, position, added_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            rusqlite::params![queue_id, job_id, next],
        )?;
        conn.execute(
            "UPDATE downloads SET queue_id = ?2 WHERE id = ?1",
            rusqlite::params![job_id, queue_id],
        )?;
        Ok(())
    })
}

/// Removes a download from its queue.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn dequeue(store: &Store, job_id: &str) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM queue_items WHERE job_id = ?1", [job_id])?;
        conn.execute(
            "UPDATE downloads SET queue_id = NULL WHERE id = ?1",
            [job_id],
        )?;
        Ok(())
    })
}

/// Moves a queued item within its queue by an offset (clamped).
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn move_queue_item(
    store: &Store,
    queue_id: &str,
    job_id: &str,
    offset: i64,
) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        let items: Vec<(String, i64)> = {
            let mut stmt = conn.prepare(
                "SELECT job_id, position FROM queue_items WHERE queue_id = ?1 ORDER BY position",
            )?;

            stmt.query_map([queue_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let Some(pos) = items.iter().position(|(id, _)| id == job_id) else {
            return Ok(());
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss
        )] // queue lengths are tiny
        let new_pos = {
            let target = i64::from(u32::try_from(pos).unwrap_or(u32::MAX)) + offset;
            let max = i64::try_from(items.len().saturating_sub(1)).unwrap_or(0);
            usize::try_from(target.clamp(0, max)).unwrap_or(0)
        };
        let mut reordered: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
        let item = reordered.remove(pos);
        reordered.insert(new_pos, item);
        for (i, id) in reordered.iter().enumerate() {
            conn.execute(
                "UPDATE queue_items SET position = ?3 WHERE queue_id = ?1 AND job_id = ?2",
                rusqlite::params![queue_id, id, i64::try_from(i).unwrap_or(i64::MAX)],
            )?;
        }
        Ok(())
    })
}

/// Loads a queue's ordered job ids.
#[must_use]
pub fn queue_order(store: &Store, queue_id: &str) -> Vec<String> {
    store
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT job_id FROM queue_items WHERE queue_id = ?1 ORDER BY position")?;
            let rows = stmt
                .query_map([queue_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap_or_default()
}

/// Sets a queue's active flag.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn set_queue_active(store: &Store, queue_id: &str, active: bool) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE queues SET is_active = ?2 WHERE id = ?1",
            rusqlite::params![queue_id, i64::from(active)],
        )?;
        Ok(())
    })
}

// ── Settings ─────────────────────────────────────────────────────────────

/// Reads a JSON setting.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn get_setting(store: &Store, key: &str) -> Result<Option<String>, StoreError> {
    store.with_conn(|conn| {
        let v: Option<String> = conn
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v)
    })
}

/// Writes a JSON setting.
///
/// # Errors
///
/// Returns [`StoreError`] on SQL failure.
pub fn set_setting(store: &Store, key: &str, value_json: &str) -> Result<(), StoreError> {
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO settings (key, value_json, updated_at) \
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             ON CONFLICT(key) DO UPDATE SET value_json = ?2, updated_at = \
             strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            rusqlite::params![key, value_json],
        )?;
        Ok(())
    })
}

/// Default downloads base dir (OS Downloads folder).
#[must_use]
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SwiftFetch")
}
