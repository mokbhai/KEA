use std::path::{Path, PathBuf};

use crate::registry::{OnnxBundleShape, OnnxModelEntry};

/// Rejects ids that could escape the storage root: `root.join(id)` replaces
/// the root entirely for absolute paths and `..`/separators walk out of it.
fn validate_removable_id(model_id: &str) -> std::io::Result<()> {
    if model_id.contains('/') || model_id.contains('\\') || model_id.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid model id: {model_id}"),
        ));
    }
    Ok(())
}

pub struct ModelStorage {
    pub root: PathBuf,
}

impl ModelStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn path_for(&self, model_id: &str) -> PathBuf {
        self.root.join(format!("{model_id}.gguf"))
    }

    pub fn is_installed(&self, model_id: &str) -> bool {
        let path = self.path_for(model_id);
        path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
    }

    pub fn installed_models(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(_) => return ids,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".gguf") else {
                continue;
            };
            if path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        ids
    }

    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    pub fn default_whisper_root(app_data: &Path) -> PathBuf {
        app_data.join("models").join("whisper")
    }

    pub fn onnx_dir_for(&self, model_id: &str) -> PathBuf {
        self.root.join(model_id)
    }

    /// Whether a `tokens.txt`-rooted bundle is installed.
    ///
    /// Kept for the callers that only have an id — every STT and TTS bundle
    /// in the tree is this shape. Anything that has the catalog entry in hand
    /// should use [`ModelStorage::is_onnx_entry_installed`] instead, which
    /// asks the entry's own shape.
    pub fn is_onnx_installed(&self, model_id: &str) -> bool {
        self.is_onnx_bundle_installed(model_id, &OnnxBundleShape::TokensBundle)
    }

    /// Whether the asset this catalog entry describes is on disk.
    ///
    /// Dispatches through the entry's [`OnnxBundleShape`], which is the same
    /// value the installer dispatched on — so a model that installs correctly
    /// cannot then report itself missing forever, which is exactly what a
    /// second hardcoded `tokens.txt` check here would have caused.
    pub fn is_onnx_entry_installed(&self, entry: &OnnxModelEntry) -> bool {
        self.is_onnx_bundle_installed(&entry.id, &entry.bundle)
    }

    pub fn is_onnx_bundle_installed(&self, model_id: &str, shape: &OnnxBundleShape) -> bool {
        let dir = self.onnx_dir_for(model_id);
        dir.is_dir() && dir.join(shape.marker()).is_file()
    }

    /// Remove an installed whisper model file. Removing a model that is not
    /// installed is a no-op.
    pub fn remove_model(&self, model_id: &str) -> std::io::Result<()> {
        validate_removable_id(model_id)?;
        match std::fs::remove_file(self.path_for(model_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Remove an installed ONNX model directory. Removing a model that is not
    /// installed is a no-op.
    pub fn remove_onnx(&self, model_id: &str) -> std::io::Result<()> {
        validate_removable_id(model_id)?;
        match std::fs::remove_dir_all(self.onnx_dir_for(model_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn default_parakeet_root(app_data: &Path) -> PathBuf {
        app_data.join("models").join("parakeet")
    }

    pub fn default_tts_root(app_data: &Path) -> PathBuf {
        app_data.join("models").join("tts")
    }

    /// A root of its own rather than a corner of the parakeet one: the app's
    /// kind-to-root map is 1:1, and keeping it that way is what lets listing
    /// and delete stay kind-driven instead of learning to tell two families
    /// apart inside one directory.
    pub fn default_streaming_root(app_data: &Path) -> PathBuf {
        app_data.join("models").join("streaming")
    }

    /// See [`ModelStorage::default_streaming_root`] for why every kind gets
    /// its own root.
    pub fn default_diarization_root(app_data: &Path) -> PathBuf {
        app_data.join("models").join("diarization")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_for_model_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let path = storage.path_for("ggml-base.en");
        assert!(path.ends_with("ggml-base.en.gguf"));
        assert!(!storage.is_installed("ggml-base.en"));
        std::fs::write(&path, b"fake").unwrap();
        assert!(storage.is_installed("ggml-base.en"));
        assert_eq!(storage.installed_models(), vec!["ggml-base.en".to_string()]);
    }

    #[test]
    fn installed_models_ignores_non_gguf_files() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        std::fs::write(storage.root.join("readme.txt"), b"hi").unwrap();
        std::fs::write(storage.path_for("ggml-small.en"), b"x").unwrap();
        assert_eq!(
            storage.installed_models(),
            vec!["ggml-small.en".to_string()]
        );
    }

    #[test]
    fn default_whisper_root_under_app_data() {
        let root = ModelStorage::default_whisper_root(Path::new("/tmp/kea"));
        assert!(root.ends_with("models/whisper"));
    }

    /// One root per kind, and no two the same — a shared root would make
    /// `installed_models` for one family list another's.
    #[test]
    fn every_model_root_is_distinct() {
        let app_data = Path::new("/tmp/kea");
        let roots = [
            ModelStorage::default_whisper_root(app_data),
            ModelStorage::default_parakeet_root(app_data),
            ModelStorage::default_tts_root(app_data),
            ModelStorage::default_streaming_root(app_data),
            ModelStorage::default_diarization_root(app_data),
        ];
        assert!(roots[3].ends_with("models/streaming"));
        assert!(roots[4].ends_with("models/diarization"));
        let unique: std::collections::HashSet<_> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len());
    }

    #[test]
    fn remove_model_deletes_file_and_tolerates_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        std::fs::write(storage.path_for("ggml-base.en"), b"fake").unwrap();
        assert!(storage.is_installed("ggml-base.en"));
        storage.remove_model("ggml-base.en").unwrap();
        assert!(!storage.is_installed("ggml-base.en"));
        // removing again is a no-op
        storage.remove_model("ggml-base.en").unwrap();
    }

    #[test]
    fn remove_onnx_deletes_dir_and_tolerates_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let model_dir = storage.onnx_dir_for("vits-piper-en-us-lessac-medium");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();
        assert!(storage.is_onnx_installed("vits-piper-en-us-lessac-medium"));
        storage
            .remove_onnx("vits-piper-en-us-lessac-medium")
            .unwrap();
        assert!(!storage.is_onnx_installed("vits-piper-en-us-lessac-medium"));
        assert!(!model_dir.exists());
        // removing again is a no-op
        storage
            .remove_onnx("vits-piper-en-us-lessac-medium")
            .unwrap();
    }

    #[test]
    fn remove_model_rejects_traversal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().join("models"));
        std::fs::create_dir_all(&storage.root).unwrap();
        let outside = dir.path().join("outside.gguf");
        std::fs::write(&outside, b"keep me").unwrap();

        for id in ["../outside", "..", "a/b", "a\\b", "/etc/passwd"] {
            let err = storage.remove_model(id).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "id: {id}");
        }
        assert!(outside.exists(), "file outside the root must survive");
    }

    #[test]
    fn remove_onnx_rejects_traversal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().join("models"));
        std::fs::create_dir_all(&storage.root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        for id in ["../outside", "..", "a/b", "a\\b", "/tmp"] {
            let err = storage.remove_onnx(id).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "id: {id}");
        }
        assert!(outside.exists(), "dir outside the root must survive");
    }

    #[test]
    fn onnx_dir_install_marker() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        assert!(!storage.is_onnx_installed("parakeet-tdt-0.6b-v2"));
        let model_dir = storage.onnx_dir_for("parakeet-tdt-0.6b-v2");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();
        assert!(storage.is_onnx_installed("parakeet-tdt-0.6b-v2"));
    }
}
