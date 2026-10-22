//! haloid-media — what an upload is, and its derivatives.
//!
//! [`MediaToolkit`] implements `haloid_core::processor::MediaProcessor`.
//! Probing sniffs the real type from the bytes, then reads images from their
//! headers in-process and video and audio through ffprobe. Everything that
//! touches pixels — derivatives, resizing, re-encoding — runs `vips` and
//! `ffmpeg` as subprocesses, never libvips in-process: a crash in a decoder
//! fed a hostile file kills one child process, not the server.
//!
//! A semaphore sized to the CPU count bounds how many files are worked on at
//! once; byte ingestion happens before this crate is involved and is never
//! gated by it.

mod av;
mod derive;
mod exif;
mod heif;
mod image;
mod sniff;
mod strip;
mod tool;
mod watermark;

use async_trait::async_trait;
use haloid_core::MediaError;
use haloid_core::media::{MediaKind, ProbeResult};
use haloid_core::processor::{DerivativePlan, DerivativeSet, MediaProcessor, Watermark};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// The tool binaries; bare names are looked up on the PATH.
    pub ffprobe: PathBuf,
    pub ffmpeg: PathBuf,
    pub vips: PathBuf,
    /// Wall-clock limit for one external tool run, after which it is killed.
    pub timeout: Duration,
    /// How many files may be probed or processed at once.
    pub concurrency: usize,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ffprobe: "ffprobe".into(),
            ffmpeg: "ffmpeg".into(),
            vips: "vips".into(),
            timeout: Duration::from_secs(30),
            concurrency: std::thread::available_parallelism().map_or(4, |n| n.get()),
        }
    }
}

pub struct MediaToolkit {
    config: MediaConfig,
    permits: Arc<Semaphore>,
}

impl MediaToolkit {
    pub fn new(config: MediaConfig) -> Self {
        let permits = Arc::new(Semaphore::new(config.concurrency.max(1)));
        Self { config, permits }
    }

    async fn permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, MediaError> {
        self.permits
            .acquire()
            .await
            .map_err(|_| MediaError::ToolUnavailable("media semaphore"))
    }

    async fn probe_unpermitted(&self, path: &Path) -> Result<ProbeResult, MediaError> {
        let owned = path.to_path_buf();
        let sniffed = tokio::task::spawn_blocking(move || read_head(&owned))
            .await
            .map_err(|e| MediaError::ProbeFailed(e.to_string()))?
            .and_then(|head| sniff::sniff(&head))?;

        match sniffed.kind {
            MediaKind::Image => {
                let owned = path.to_path_buf();
                tokio::task::spawn_blocking(move || image::probe(&owned, sniffed))
                    .await
                    .map_err(|e| MediaError::ProbeFailed(e.to_string()))?
            }
            MediaKind::Video | MediaKind::Audio => {
                let path = std::path::absolute(path).map_err(io)?;
                av::probe(&self.config.ffprobe, self.config.timeout, &path, sniffed).await
            }
        }
    }
}

fn io(error: std::io::Error) -> MediaError {
    MediaError::ProbeFailed(format!("could not read the upload: {error}"))
}

/// The leading bytes that sniffing needs.
fn read_head(path: &Path) -> Result<Vec<u8>, MediaError> {
    let mut head = Vec::with_capacity(sniff::HEAD_LEN);
    std::fs::File::open(path)
        .map_err(io)?
        .take(sniff::HEAD_LEN as u64)
        .read_to_end(&mut head)
        .map_err(io)?;
    Ok(head)
}

#[async_trait]
impl MediaProcessor for MediaToolkit {
    /// Never decodes pixels; see the `image` module. The caller compares
    /// [`ProbeResult::pixels`] against its limit before deriving anything.
    async fn probe(&self, path: &Path) -> Result<ProbeResult, MediaError> {
        let _permit = self.permit().await?;
        self.probe_unpermitted(path).await
    }

    async fn derive(
        &self,
        path: &Path,
        probe: &ProbeResult,
        plan: &DerivativePlan,
    ) -> Result<DerivativeSet, MediaError> {
        let _permit = self.permit().await?;
        let path = std::path::absolute(path).map_err(io)?;
        derive::derive(&self.config, &path, probe, plan).await
    }

    async fn resize(&self, path: &Path, max_width: u32) -> Result<PathBuf, MediaError> {
        let _permit = self.permit().await?;
        let path = std::path::absolute(path).map_err(io)?;
        let probe = self.probe_unpermitted(&path).await?;
        derive::resize(&self.config, &path, &probe, max_width).await
    }

    async fn watermark(&self, path: &Path, mark: &Watermark) -> Result<PathBuf, MediaError> {
        let _permit = self.permit().await?;
        let path = std::path::absolute(path).map_err(io)?;
        let probe = self.probe_unpermitted(&path).await?;
        let vips = tool::Tool {
            binary: &self.config.vips,
            name: "vips",
            timeout: self.config.timeout,
        };
        let ffmpeg = tool::Tool {
            binary: &self.config.ffmpeg,
            name: "ffmpeg",
            timeout: self.config.timeout,
        };
        watermark::stamp(&vips, &ffmpeg, &path, &probe, mark).await
    }

    async fn strip_metadata(&self, path: &Path, probe: &ProbeResult) -> Result<(), MediaError> {
        let _permit = self.permit().await?;
        let path = std::path::absolute(path).map_err(io)?;
        strip::strip(&self.config, &path, probe).await
    }

    async fn read_exif(&self, path: &Path) -> Result<Option<String>, MediaError> {
        let owned = path.to_path_buf();
        tokio::task::spawn_blocking(move || exif::capture(&owned))
            .await
            .map_err(|e| MediaError::ProbeFailed(e.to_string()))
    }
}
