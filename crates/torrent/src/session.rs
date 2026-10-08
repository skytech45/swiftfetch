//! Managed `BitTorrent` sessions on top of `librqbit` (Milestone 6).
//!
//! [`TorrentEngine`] owns one `librqbit` [`Session`](librqbit::Session):
//! torrents are added from magnet links or `.torrent` bytes, progress and
//! peer counts come from librqbit stats, and the seeding-ratio policy
//! (default: stop at 1.0) is enforced by [`seeding_complete`] — the caller
//! polls [`TorrentEngine::status`] and removes finished torrents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// librqbit torrent id (`usize`, mirrors `librqbit::session::TorrentId`).
pub type TorrentId = usize;

/// Session failure.
#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    /// librqbit rejected the add / operation.
    #[error("torrent backend error: {0}")]
    Backend(#[from] anyhow::Error),
    /// Unknown torrent id.
    #[error("unknown torrent `{0}`")]
    UnknownTorrent(TorrentId),
    /// The source is not a magnet link or `.torrent` file.
    #[error("not a magnet link or .torrent file")]
    BadSource,
}

/// Live status of one torrent.
#[derive(Debug, Clone)]
pub struct TorrentStatus {
    /// librqbit torrent id.
    pub id: TorrentId,
    /// Display name, when metadata is resolved.
    pub name: Option<String>,
    /// Bytes downloaded (checked).
    pub progress_bytes: u64,
    /// Total content bytes.
    pub total_bytes: u64,
    /// Bytes uploaded (for the ratio policy).
    pub uploaded_bytes: u64,
    /// Download speed (bytes/s).
    pub down_bps: u64,
    /// Upload speed (bytes/s).
    pub up_bps: u64,
    /// Live peer connections.
    pub peers: u32,
    /// Fully downloaded.
    pub finished: bool,
    /// Paused by the user.
    pub paused: bool,
}

/// Whether the seeding-ratio policy is satisfied: stop seeding once
/// `uploaded >= downloaded * ratio` (a torrent with nothing downloaded yet
/// never satisfies the policy — it must finish first).
#[must_use]
#[allow(clippy::cast_precision_loss)] // byte counts are far below 2^53
pub fn seeding_complete(uploaded_bytes: u64, downloaded_bytes: u64, ratio: f64) -> bool {
    if downloaded_bytes == 0 || ratio <= 0.0 {
        return false;
    }
    uploaded_bytes as f64 >= downloaded_bytes as f64 * ratio
}

/// A managed `BitTorrent` session: one `librqbit` session plus the id→name map
/// for torrents added through this engine.
pub struct TorrentEngine {
    session: std::sync::Arc<librqbit::Session>,
    output_dir: PathBuf,
    names: Mutex<HashMap<TorrentId, String>>,
    paused: Mutex<HashMap<TorrentId, bool>>,
}

impl TorrentEngine {
    /// Opens a session downloading into `output_dir`.
    ///
    /// * `enable_dht` — DHT + LSD discovery (disable in tests / offline).
    /// * `disable_trackers` — no tracker announces (hermetic tests).
    /// * `listen_addr` — TCP listen socket for incoming peers (`None` =
    ///   no listening socket; pass `127.0.0.1:0` for loopback seeding).
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError::Backend`] when the session cannot start.
    pub async fn open(
        output_dir: &Path,
        enable_dht: bool,
        disable_trackers: bool,
        listen_addr: Option<std::net::SocketAddr>,
    ) -> Result<Self, TorrentError> {
        let opts = librqbit::SessionOptions {
            dht: enable_dht.then(librqbit::DhtSessionConfig::default),
            disable_trackers,
            persistence: None,
            listen: listen_addr.map(|addr| librqbit::ListenerOptions {
                listen_addr: addr,
                ipv4_only: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let session = librqbit::Session::new_with_opts(output_dir.to_path_buf(), opts).await?;
        Ok(Self {
            session,
            output_dir: output_dir.to_path_buf(),
            names: Mutex::new(HashMap::new()),
            paused: Mutex::new(HashMap::new()),
        })
    }

    /// The bound TCP listen address, when listening is enabled.
    #[must_use]
    pub fn listen_addr(&self) -> Option<std::net::SocketAddr> {
        self.session.listen_addr()
    }

    /// Adds a magnet link. Returns the torrent id once metadata resolves
    /// enough to track (librqbit resolves pure magnets via DHT/trackers).
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError`] when the link is not a magnet or the
    /// backend rejects it.
    pub async fn add_magnet(
        &self,
        link: &str,
        output_subdir: Option<String>,
        initial_peers: Vec<std::net::SocketAddr>,
    ) -> Result<TorrentId, TorrentError> {
        let magnet = crate::magnet::parse_magnet(link).map_err(|_| TorrentError::BadSource)?;
        let name = magnet
            .name
            .clone()
            .unwrap_or_else(|| format!("magnet-{}", &magnet.info_hash_hex[..8]));
        let opts = librqbit::AddTorrentOptions {
            overwrite: true,
            initial_peers: Some(initial_peers).filter(|peers| !peers.is_empty()),
            sub_folder: output_subdir,
            ..Default::default()
        };
        let response = self
            .session
            .add_torrent(
                librqbit::AddTorrent::Url(std::borrow::Cow::Owned(link.to_owned())),
                Some(opts),
            )
            .await?;
        self.track(response, name)
    }

    /// Adds a `.torrent` file's bytes (validates them as v1 metainfo first).
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError`] when the bytes are not a torrent or the
    /// backend rejects them.
    pub async fn add_torrent_bytes(
        &self,
        bytes: &[u8],
        output_subdir: Option<String>,
        initial_peers: Vec<std::net::SocketAddr>,
    ) -> Result<TorrentId, TorrentError> {
        let meta = crate::metainfo::parse_torrent(bytes).map_err(|_| TorrentError::BadSource)?;
        let opts = librqbit::AddTorrentOptions {
            overwrite: true,
            initial_peers: Some(initial_peers).filter(|peers| !peers.is_empty()),
            sub_folder: output_subdir,
            ..Default::default()
        };
        let response = self
            .session
            .add_torrent(
                librqbit::AddTorrent::TorrentFileBytes(bytes::Bytes::copy_from_slice(bytes)),
                Some(opts),
            )
            .await?;
        self.track(response, meta.name.clone())
    }

    fn track(
        &self,
        response: librqbit::AddTorrentResponse,
        name: String,
    ) -> Result<TorrentId, TorrentError> {
        let Some(handle) = response.into_handle() else {
            return Err(TorrentError::Backend(anyhow::anyhow!(
                "backend returned no handle"
            )));
        };
        let id = handle.id();
        self.names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, name);
        Ok(id)
    }

    /// Current status of one torrent.
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError::UnknownTorrent`] for unlisted ids.
    pub fn status(&self, id: TorrentId) -> Result<TorrentStatus, TorrentError> {
        let handle = self
            .find_handle(id)
            .ok_or(TorrentError::UnknownTorrent(id))?;
        let stats = handle.stats();
        let (down_bps, up_bps, peers) = stats.live.as_ref().map_or((0, 0, 0), |live| {
            (
                live.download_speed.as_bytes(),
                live.upload_speed.as_bytes(),
                live.snapshot.peer_stats.live,
            )
        });
        let paused = self
            .paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .copied()
            .unwrap_or(false);
        Ok(TorrentStatus {
            id,
            name: self
                .names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .cloned()
                .or_else(|| handle.name()),
            progress_bytes: stats.progress_bytes,
            total_bytes: stats.total_bytes,
            uploaded_bytes: stats.uploaded_bytes,
            down_bps,
            up_bps,
            peers,
            finished: stats.finished,
            paused,
        })
    }

    /// Pauses a torrent (keeps its data and place in the session).
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError`] when the backend refuses.
    pub async fn pause(&self, id: TorrentId) -> Result<(), TorrentError> {
        let handle = self
            .find_handle(id)
            .ok_or(TorrentError::UnknownTorrent(id))?;
        self.session.pause(&handle).await?;
        self.paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, true);
        Ok(())
    }

    /// Resumes a paused torrent.
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError`] when the backend refuses.
    pub async fn resume(&self, id: TorrentId) -> Result<(), TorrentError> {
        let handle = self
            .find_handle(id)
            .ok_or(TorrentError::UnknownTorrent(id))?;
        self.session.unpause(&handle).await?;
        self.paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, false);
        Ok(())
    }

    /// Removes a torrent from the session (`delete_files` also wipes data).
    ///
    /// # Errors
    ///
    /// Returns [`TorrentError`] when the backend refuses.
    pub async fn remove(&self, id: TorrentId, delete_files: bool) -> Result<(), TorrentError> {
        use librqbit::api::TorrentIdOrHash;
        self.session
            .delete(TorrentIdOrHash::Id(id), delete_files)
            .await?;
        self.names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        self.paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        Ok(())
    }

    /// Default output directory of the session.
    #[must_use]
    pub fn output_dir(&self) -> PathBuf {
        self.output_dir.clone()
    }

    /// Clones the handle for `id` (`with_torrents` hands out a trait
    /// object, so `Iterator::find` is unavailable — walk it manually).
    #[allow(clippy::while_let_on_iterator)] // trait objects cannot `for`
    fn find_handle(&self, id: TorrentId) -> Option<std::sync::Arc<librqbit::ManagedTorrent>> {
        self.session.with_torrents(|torrents| {
            let mut found = None;
            while let Some((torrent_id, handle)) = torrents.next() {
                if torrent_id == id {
                    found = Some(handle.clone());
                    break;
                }
            }
            found
        })
    }
}
