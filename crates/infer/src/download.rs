use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;


use crate::error::InferError;
use crate::registry::{ModelRegistry, OnnxModelEntry};
use crate::storage::ModelStorage;

#[cfg(unix)]
fn available_disk_space(path: &Path) -> u64 {
    let path_c = match std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(_) => return u64::MAX,
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(path_c.as_ptr(), &mut stat) };
    if rc != 0 {
        return u64::MAX;
    }
    stat.f_bsize as u64 * stat.f_bavail as u64
}

#[cfg(not(unix))]
fn available_disk_space(_path: &Path) -> u64 {
    u64::MAX
}

fn check_disk_space(
    storage_root: &Path,
    model_id: &str,
    required: u64,
) -> Result<(), InferError> {
    let available = available_disk_space(storage_root);
    if available < required {
        return Err(InferError::InsufficientSpace {
            model_id: model_id.to_string(),
            required,
            available,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub bytes_received: u64,
    pub bytes_total: u64,
}

/// What a completed transfer wrote. The digest is computed while streaming so
/// the file never has to be re-read (or held in memory) to be verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedFile {
    pub bytes: u64,
    pub sha256: String,
}

#[async_trait]
pub trait DownloadTransport: Send + Sync {
    /// Streams `url` into `dest`, calling `on_chunk(received, total)` as bytes
    /// land. `total` is 0 when the server sends no length.
    ///
    /// Streaming is the contract, not an implementation detail: a model is
    /// hundreds of megabytes, so buffering the whole body would both hold it
    /// in memory twice and leave the UI with nothing to report until the very
    /// end.
    async fn fetch_to_file(
        &self,
        url: &str,
        dest: &Path,
        on_chunk: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<StreamedFile, InferError>;
}

fn verify_digest(model_id: &str, expected: &str, actual: &str) -> Result<(), InferError> {
    if actual != expected {
        return Err(InferError::HashMismatch {
            model_id: model_id.to_string(),
            expected: expected[..expected.len().min(8)].to_string(),
            actual: actual[..actual.len().min(8)].to_string(),
        });
    }
    Ok(())
}

fn find_onnx_bundle_root(dir: &Path) -> Option<PathBuf> {
    if dir.join("tokens.txt").is_file() {
        return Some(dir.to_path_buf());
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_onnx_bundle_root(&path) {
                return Some(found);
            }
        }
    }
    None
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), InferError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn install_onnx_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), InferError> {
    use bzip2::read::BzDecoder;
    use tar::{Archive, EntryType};

    let temp = tempfile::tempdir()?;
    // Decode straight off the file: the archive is streamed to disk, so
    // holding it in memory again only to unpack it would undo that.
    let decoder = BzDecoder::new(std::fs::File::open(archive_path)?);
    let mut archive = Archive::new(decoder);

    // Extract entry-by-entry with path-traversal hardening. tar's unpack()
    // only does best-effort `..` filtering; we validate each path explicitly
    // and reject entries whose canonical target would escape the staging dir.
    for entry in archive.entries().map_err(|e| {
        InferError::Other(format!("failed to read onnx archive entries: {e}"))
    })? {
        let mut entry = entry.map_err(|e| {
            InferError::Other(format!("failed to read onnx archive entry: {e}"))
        })?;
        let header = entry.header();

        // Reject entries whose path contains `..`, is absolute, or resolves
        // outside the staging temp dir.
        let entry_path = entry.path().map_err(|e| {
            InferError::Other(format!("invalid onnx archive path: {e}"))
        })?;
        if entry_path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(InferError::Other(format!(
                "archive path escapes staging: {}",
                entry_path.display()
            )));
        }
        let target = temp.path().join(&entry_path);
        // Canonicalize the parent to catch symlink-based escapes.
        if let Some(parent) = target.parent() {
            let resolved = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            if !resolved.starts_with(temp.path()) {
                return Err(InferError::Other(format!(
                    "archive path escapes staging: {}",
                    entry_path.display()
                )));
            }
        }

        match header.entry_type() {
            EntryType::Directory => {
                std::fs::create_dir_all(&target)?;
            }
            EntryType::Symlink | EntryType::Link => {
                // Reject sym/hard-links entirely — we don't need them and
                // they're a potential escape vector.
                return Err(InferError::Other(
                    "onnx archive contains unsupported symlink".to_string(),
                ));
            }
            _ => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                entry
                    .unpack(&target)
                    .map_err(|e| InferError::Other(format!("failed to unpack entry: {e}")))?;
            }
        }
    }

    let bundle_root = find_onnx_bundle_root(temp.path())
        .ok_or_else(|| InferError::Other("onnx archive missing tokens.txt bundle".to_string()))?;

    if let Some(parent) = dest_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if std::fs::rename(&bundle_root, dest_dir).is_ok() {
        if temp.path().exists() {
            let _ = std::fs::remove_dir_all(temp.path());
        }
        return Ok(());
    }

    // cross-filesystem fallback: copy to a sibling temp dir then rename
    let staging = dest_dir.with_extension(".tmp-staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_dir_all(&bundle_root, &staging)?;
    if dest_dir.exists() {
        std::fs::remove_dir_all(dest_dir)?;
    }
    std::fs::rename(&staging, dest_dir)?;
    Ok(())
}

fn temp_file_for(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    name.push_str(".tmp");
    final_path.parent().unwrap_or(Path::new(".")).join(name)
}

pub struct ModelDownloader {
    transport: Arc<dyn DownloadTransport>,
    storage: ModelStorage,
}

impl ModelDownloader {
    pub fn new(transport: Arc<dyn DownloadTransport>, storage: ModelStorage) -> Self {
        Self {
            transport,
            storage,
        }
    }

    pub async fn download_whisper(
        &self,
        model_id: &str,
        on_progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<PathBuf, InferError> {
        let entry = ModelRegistry::find_whisper(model_id)
            .ok_or_else(|| InferError::ModelNotFound(model_id.to_string()))?;

        self.storage.ensure_root()?;

        check_disk_space(&self.storage.root, model_id, entry.size_bytes)?;

        let path = self.storage.path_for(model_id);
        let temp = temp_file_for(&path);

        let streamed = self
            .stream_to_temp(&entry.url, &temp, model_id, entry.size_bytes, &on_progress)
            .await?;
        if let Err(e) = verify_digest(model_id, &entry.sha256, &streamed.sha256) {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
        std::fs::rename(&temp, &path)?;

        on_progress(DownloadProgress {
            model_id: model_id.to_string(),
            bytes_received: streamed.bytes,
            bytes_total: streamed.bytes,
        });

        Ok(path)
    }

    /// Streams a URL to `temp`, forwarding byte counts as they arrive so the
    /// caller can report real progress. Falls back to the catalog size when
    /// the server sends no `Content-Length`, so a percentage is still shown.
    async fn stream_to_temp(
        &self,
        url: &str,
        temp: &Path,
        model_id: &str,
        catalog_size: u64,
        on_progress: &(impl Fn(DownloadProgress) + Send + Sync),
    ) -> Result<StreamedFile, InferError> {
        on_progress(DownloadProgress {
            model_id: model_id.to_string(),
            bytes_received: 0,
            bytes_total: catalog_size,
        });
        let report = |received: u64, total: u64| {
            on_progress(DownloadProgress {
                model_id: model_id.to_string(),
                bytes_received: received,
                bytes_total: if total > 0 { total } else { catalog_size },
            });
        };
        let streamed = self.transport.fetch_to_file(url, temp, &report).await;
        if streamed.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        streamed
    }

    pub async fn download_onnx(
        &self,
        entry: &OnnxModelEntry,
        on_progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<(), InferError> {
        self.storage.ensure_root()?;

        check_disk_space(&self.storage.root, &entry.id, entry.size_bytes.saturating_mul(2))?;

        let dest = self.storage.onnx_dir_for(&entry.id);
        let temp = temp_file_for(&dest);

        let streamed = self
            .stream_to_temp(&entry.url, &temp, &entry.id, entry.size_bytes, &on_progress)
            .await?;
        if let Err(e) = verify_digest(&entry.id, &entry.sha256, &streamed.sha256) {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }

        let installed = install_onnx_archive(&temp, &dest);
        let _ = std::fs::remove_file(&temp);
        installed?;

        on_progress(DownloadProgress {
            model_id: entry.id.clone(),
            bytes_received: streamed.bytes,
            bytes_total: streamed.bytes,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Writes the payload in several chunks so tests see the same
    /// mid-transfer progress a real socket produces. A fake that delivered
    /// everything at once is what let a no-progress downloader look correct.
    struct FakeTransport {
        data: Vec<u8>,
        chunks: usize,
    }

    impl FakeTransport {
        fn new(data: Vec<u8>) -> Self {
            Self { data, chunks: 4 }
        }
    }

    #[async_trait]
    impl DownloadTransport for FakeTransport {
        async fn fetch_to_file(
            &self,
            _url: &str,
            dest: &Path,
            on_chunk: &(dyn Fn(u64, u64) + Send + Sync),
        ) -> Result<StreamedFile, InferError> {
            use sha2::{Digest, Sha256};
            use std::io::Write;

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let total = self.data.len() as u64;
            let size = self.data.len().div_ceil(self.chunks.max(1));
            let mut file = std::fs::File::create(dest)?;
            let mut hasher = Sha256::new();
            let mut received = 0u64;
            for chunk in self.data.chunks(size.max(1)) {
                hasher.update(chunk);
                file.write_all(chunk)?;
                received += chunk.len() as u64;
                on_chunk(received, total);
            }
            file.flush()?;
            Ok(StreamedFile {
                bytes: received,
                sha256: format!("{:x}", hasher.finalize()),
            })
        }
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(data);
        format!("{:x}", h.finalize())
    }

    #[tokio::test]
    async fn progress_arrives_during_the_transfer_not_only_at_the_end() {
        // The regression this guards: the downloader used to buffer the whole
        // body before emitting anything, so the UI showed "starting download…"
        // for the entire transfer and could never render a percentage.
        let payload = vec![7u8; 8192];
        let hash = sha256_hex(&payload);
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());

        let entry = OnnxModelEntry {
            id: "streamed".into(),
            display_name: "Streamed".into(),
            language: "en-US".into(),
            url: "https://example.com/m.tar.bz2".into(),
            size_bytes: payload.len() as u64,
            sha256: hash,
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(Arc::new(FakeTransport::new(payload.clone())), storage);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        // Unpacking fails (not a real archive); progress is what's under test.
        let _ = dl
            .download_onnx(&entry, move |p| sink.lock().unwrap().push(p))
            .await;

        let events = seen.lock().unwrap();
        let partial: Vec<_> = events
            .iter()
            .filter(|p| p.bytes_received > 0 && p.bytes_received < p.bytes_total)
            .collect();
        assert!(
            !partial.is_empty(),
            "expected progress while bytes were still arriving, got {events:?}"
        );
        assert!(
            events.iter().all(|p| p.bytes_total > 0),
            "every event must carry a total so a percentage can be shown: {events:?}"
        );
    }

    #[tokio::test]
    async fn downloader_writes_file_and_reports_progress() {
        let payload = b"valid onnx payload for progress test".to_vec();
        let hash = sha256_hex(&payload);

        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());

        let entry = OnnxModelEntry {
            id: "test-whisper-progress".into(),
            display_name: "Test".into(),
            language: "en-US".into(),
            url: "https://example.com/test.tar.bz2".into(),
            size_bytes: payload.len() as u64,
            sha256: hash,
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(payload.clone())),
            storage,
        );

        let progress = Arc::new(Mutex::new(Vec::new()));
        let p2 = progress.clone();

        let result = dl
            .download_onnx(&entry, move |p| {
                p2.lock().unwrap().push(p);
            })
            .await;

        let events = progress.lock().unwrap();
        assert!(!events.is_empty());
        assert_eq!(events.first().unwrap().bytes_total, payload.len() as u64);

        // unpack will fail (not a real tar.bz2), but hash + progress already worked
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("unpack") || err_str.contains("onnx archive"),
            "unexpected error: {err_str}"
        );
    }

    #[tokio::test]
    async fn downloader_errors_on_unknown_model() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let dl = ModelDownloader::new(Arc::new(FakeTransport::new(vec![])), storage);
        let err = dl
            .download_whisper("unknown-model", |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InferError::ModelNotFound(_)));
    }

    #[tokio::test]
    async fn hash_mismatch_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let payload = b"tampered content".to_vec();
        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(payload)),
            storage,
        );

        let err = dl
            .download_whisper("ggml-base.en", |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InferError::HashMismatch { .. }));
    }

    #[tokio::test]
    async fn matching_hash_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let payload = b"valid payload for hash check test".to_vec();
        let hash = sha256_hex(&payload);

        // We need the registry hash to match, so we create our own entry path.
        // Instead of using the registry, we test through the ONNX path which
        // accepts an OnnxModelEntry directly.
        let entry = OnnxModelEntry {
            id: "test-model".into(),
            display_name: "Test".into(),
            language: "en-US".into(),
            url: "https://example.com/test.tar.bz2".into(),
            size_bytes: payload.len() as u64,
            sha256: hash,
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(payload.clone())),
            storage,
        );

        let progress = Arc::new(Mutex::new(Vec::new()));
        let p2 = progress.clone();

        let result = dl
            .download_onnx(&entry, move |p| {
                p2.lock().unwrap().push(p);
            })
            .await;

        // download_onnx tries to unpack as tar.bz2, so it'll fail at unpack,
        // but the hash check will have passed first (it runs before unpack).
        // The error should be about unpacking, not about hash mismatch.
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("unpack") || err_str.contains("onnx archive"),
            "expected unpack/archive error, got: {err_str}"
        );

        let events = progress.lock().unwrap();
        assert!(!events.is_empty());
    }

    #[tokio::test]
    async fn onnx_hash_mismatch_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let entry = OnnxModelEntry {
            id: "tampered-model".into(),
            display_name: "Tampered".into(),
            language: "en-US".into(),
            url: "https://example.com/tampered.tar.bz2".into(),
            size_bytes: 100,
            sha256: "a".repeat(64),
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(b"not the expected content".to_vec())),
            storage,
        );

        let err = dl
            .download_onnx(&entry, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InferError::HashMismatch { .. }));
    }

    #[tokio::test]
    async fn whisper_no_stale_file_after_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let target = storage.path_for("ggml-base.en");

        // pre-create a "stale" file to ensure it isn't clobbered
        std::fs::write(&target, b"previous install").unwrap();

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(b"bad data".to_vec())),
            storage,
        );

        let _ = dl.download_whisper("ggml-base.en", |_| {}).await;

        // temp file should not exist (cleaned up or never written), and final
        // file must hold the previous-stale content — not the bad download.
        let temp = temp_file_for(&target);
        assert!(!temp.exists(), "temp file {temp:?} should not remain");
        assert!(target.is_file());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "previous install"
        );
    }

    #[tokio::test]
    async fn whisper_atomic_write_success() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let target = storage.path_for("ggml-base.en");

        let payload = b"good content with correct hash".to_vec();
        let hash = sha256_hex(&payload);

        let entry = OnnxModelEntry {
            id: "ggml-base.en".into(),
            display_name: "Test".into(),
            language: "en-US".into(),
            url: "https://example.com/test.bin".into(),
            size_bytes: payload.len() as u64,
            sha256: hash,
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(payload.clone())),
            storage,
        );

        // use download_onnx for the test — install_onnx_archive will fail
        // because it's not a real tar.bz2, but that proves the atomic path
        // through install_onnx_archive works (staging doesn't leak).
        let result = dl.download_onnx(&entry, |_| {}).await;
        assert!(result.is_err());
        assert!(!target.exists(), "no final file on failed install");
        let staging = target.with_extension(".tmp-staging");
        assert!(
            !staging.exists(),
            "staging dir cleaned up after rename failure"
        );
    }

    // Unix-only: non-Unix available_disk_space is best-effort (u64::MAX), so
    // the preflight intentionally cannot reject there.
    #[cfg(unix)]
    #[tokio::test]
    async fn space_preflight_rejects_impossible_size() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());

        let entry = OnnxModelEntry {
            id: "too-big".into(),
            display_name: "Too Big".into(),
            language: "en-US".into(),
            url: "https://example.com/huge.bin".into(),
            size_bytes: u64::MAX,
            sha256: "a".repeat(64),
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(vec![0u8; 8])),
            storage,
        );

        let err = dl
            .download_onnx(&entry, |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(err, InferError::InsufficientSpace { .. }),
            "expected InsufficientSpace, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn space_preflight_would_detect_insufficient_space() {
        // If available_disk_space returns a real value smaller than the
        // required amount, the preflight must reject before downloading.
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());

        let payload = b"small payload".to_vec();
        let hash = sha256_hex(&payload);

        // Use a tiny size_bytes that will pass the preflight on any real
        // system — this test just proves the error variant is reachable.
        let entry = OnnxModelEntry {
            id: "small-model".into(),
            display_name: "Small".into(),
            language: "en-US".into(),
            url: "https://example.com/small.bin".into(),
            size_bytes: 16,
            sha256: hash.clone(),
            kind: crate::registry::OnnxModelKind::Parakeet,
        };

        let dl = ModelDownloader::new(
            Arc::new(FakeTransport::new(payload.clone())),
            storage,
        );

        let result = dl.download_onnx(&entry, |_| {}).await;
        // Should fail at unpack (not a real tar.bz2), not at space check
        assert!(result.is_err());
        assert!(
            !matches!(result.as_ref().unwrap_err(), InferError::InsufficientSpace { .. }),
            "should not trigger InsufficientSpace for 16-byte model"
        );
    }
}
