//! Shared-DB bridge used by the host: staging extension captures and
//! reading capture status. Same single-writer discipline as the CLI
//! (writes under `BEGIN IMMEDIATE`).

use std::sync::Arc;

use swiftfetch_store::Store;
use swiftfetch_store::repos;

/// Stages a capture from the extension (source `extension`), recording the
/// forwarded request context.
///
/// # Errors
///
/// Returns [`swiftfetch_store::StoreError`] on SQL failure.
#[allow(clippy::too_many_arguments)] // one staging signature for every capture kind
pub async fn stage_extension_download(
    store: &Arc<std::sync::Mutex<Store>>,
    url: &str,
    cookies: Option<&str>,
    referer: Option<&str>,
    filename: Option<&str>,
    queue: Option<&str>,
    kind: &str,
    meta_json: Option<&str>,
) -> Result<String, swiftfetch_store::StoreError> {
    let store = Arc::clone(store);
    let request = repos::StageRequest {
        url: url.to_owned(),
        filename: filename.map(str::to_owned),
        queue_id: queue.map(str::to_owned),
        start_paused: true,
        source: "extension".to_owned(),
        cookies: cookies.map(str::to_owned),
        referer: referer.map(str::to_owned),
        kind: kind.to_owned(),
        meta_json: meta_json.map(str::to_owned),
        ..repos::StageRequest::default()
    };
    tokio::task::spawn_blocking(move || {
        repos::stage_download(
            &store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &request,
        )
    })
    .await
    .map_err(|err| {
        swiftfetch_store::StoreError::Sql(rusqlite::Error::ToSqlConversionFailure(err.into()))
    })?
}

/// Reads the mapped job's status for a staged capture: `(state, done, code)`.
#[must_use]
pub async fn job_status(
    store: &Arc<std::sync::Mutex<Store>>,
    staged_id: &str,
) -> Option<(String, i64, Option<String>)> {
    use rusqlite::OptionalExtension;

    let staged_store = Arc::clone(store);
    let staged_id = staged_id.to_owned();
    let job_id = tokio::task::spawn_blocking(move || {
        let guard = staged_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.with_conn(|conn| {
            conn.query_row(
                "SELECT job_id FROM staged_downloads WHERE id = ?1",
                [staged_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
    })
    .await
    .ok()?
    .ok()??;
    // `None` mapping = the app has not claimed the capture yet.
    let job_id = job_id?;
    let row_store = Arc::clone(store);
    let row = {
        tokio::task::spawn_blocking(move || {
            repos::get_download(
                &row_store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &job_id,
            )
        })
        .await
        .ok()?
        .ok()?
    };
    row.map(|row| (row.state, row.done_bytes, row.error_code))
}
