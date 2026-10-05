//! Per-job disk writer: a single task serializes all file writes (no torn
//! writes, no shared cursor), using positioned I/O so concurrent segments are
//! safe. `done` counters only advance after the writer acks a batch; data is
//! `sync_data`-flushed at least once per second and on demand. SHA-256 and
//! MD5 are streamed during the write.

use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use crate::checksum::{Digest, StreamHasher};

/// Files larger than this are pre-allocated up front (sparse where the
/// filesystem supports it) to avoid mid-download ENOSPC surprises.
pub const PREALLOC_THRESHOLD: u64 = 100 * 1024 * 1024;

const FLUSH_INTERVAL_SECS: f64 = 1.0;

/// Messages accepted by the writer task.
pub enum WriterMsg {
    /// Write `data` at the absolute `offset`; ack on completion.
    Write {
        /// Absolute file offset.
        offset: u64,
        /// Bytes to write.
        data: Vec<u8>,
        /// Ack channel (write error, if any).
        ack: oneshot::Sender<std::io::Result<()>>,
    },
    /// Flush buffered bytes to disk; ack on completion.
    Flush {
        /// Ack channel (flush error, if any).
        ack: oneshot::Sender<std::io::Result<()>>,
    },
}

/// Handle to the per-job writer task. The original channel sender stays
/// here; segment tasks get clones from [`DiskWriter::spawn`].
pub struct DiskWriter {
    tx: mpsc::Sender<WriterMsg>,
    task: tokio::task::JoinHandle<std::io::Result<(Digest, Digest)>>,
}

fn positioned_write(file: &std::fs::File, offset: u64, data: &[u8]) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt as _;
        let mut written = 0usize;
        while written < data.len() {
            let n = file.seek_write(&data[written..], offset + written as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "positioned write made no progress",
                ));
            }
            written += n;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::FileExt as _;
        file.write_all_at(data, offset)
    }
}

impl DiskWriter {
    /// Opens (creating if needed) the partial file and spawns the writer
    /// task. The file is never truncated here — fresh downloads delete the
    /// `.sfpart` first; resume reuses it. `prealloc_len` reserves the full
    /// size when the file is fresh and large.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the file cannot be opened or
    /// pre-allocated.
    pub async fn spawn(
        part_path: PathBuf,
        prealloc_len: Option<u64>,
    ) -> std::io::Result<(Self, mpsc::Sender<WriterMsg>)> {
        let setup_path = part_path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = setup_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&setup_path)?;
            if let Some(len) = prealloc_len
                && len > PREALLOC_THRESHOLD
                && file.metadata()?.len() < len
            {
                file.set_len(len)?;
            }
            Ok(())
        })
        .await
        .map_err(|err| std::io::Error::other(err.to_string()))??;

        let (tx, mut rx) = mpsc::channel::<WriterMsg>(32);

        let task = tokio::spawn(async move {
            let open = || std::fs::OpenOptions::new().write(true).open(&part_path);
            let run = async {
                let file = open()?;
                let mut hasher = StreamHasher::default();
                let mut last_flush = tokio::time::Instant::now();
                let mut dirty = false;
                let result: std::io::Result<(Digest, Digest)> = loop {
                    let msg = tokio::select! {
                        m = rx.recv() => m,
                        () = tokio::time::sleep_until(last_flush + std::time::Duration::from_secs_f64(FLUSH_INTERVAL_SECS)), if dirty => {
                            // Periodic durability tick.
                            if let Err(err) = tokio::task::block_in_place(|| file.sync_data()) {
                                break Err(err);
                            }
                            dirty = false;
                            last_flush = tokio::time::Instant::now();
                            continue;
                        }
                    };
                    match msg {
                        Some(WriterMsg::Write { offset, data, ack }) => {
                            eprintln!("[writer] write {} at {}", data.len(), offset);
                            let res = tokio::task::block_in_place(|| {
                                positioned_write(&file, offset, &data).map(|()| {
                                    hasher.update(&data);
                                })
                            });
                            dirty = true;
                            let failed = res.is_err();
                            let _ = ack.send(res);
                            if failed {
                                break Err(std::io::Error::other("write failed; writer stopping"));
                            }
                        }
                        Some(WriterMsg::Flush { ack }) => {
                            let res = tokio::task::block_in_place(|| file.sync_data());
                            dirty = false;
                            last_flush = tokio::time::Instant::now();
                            let _ = ack.send(res);
                        }
                        None => {
                            file.sync_data()?;
                            break Ok(hasher.finalize());
                        }
                    }
                };
                result
            };
            run.await
        });

        Ok((
            Self {
                tx: tx.clone(),
                task,
            },
            tx,
        ))
    }

    /// Flushes pending writes.
    ///
    /// # Errors
    ///
    /// Returns the flush error, if any.
    pub async fn flush(&self) -> std::io::Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(WriterMsg::Flush { ack: ack_tx })
            .await
            .map_err(|_| std::io::Error::other("writer closed"))?;
        ack_rx
            .await
            .map_err(|_| std::io::Error::other("writer dropped ack"))?
    }

    /// Drops the channel, waits for the writer to flush and exit, and
    /// returns the streamed digests.
    ///
    /// # Errors
    ///
    /// Returns any write error the task recorded.
    pub async fn finalize(self) -> std::io::Result<(Digest, Digest)> {
        drop(self.tx); // closing the channel ends the writer loop
        match self.task.await {
            Ok(result) => result,
            Err(err) => Err(std::io::Error::other(err.to_string())),
        }
    }
}

/// Deletes a partial file; `NotFound` is treated as success so fresh
/// downloads can call this unconditionally.
///
/// # Errors
///
/// Returns any other I/O error (permissions, directory instead of file…).
pub fn remove_partial(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
