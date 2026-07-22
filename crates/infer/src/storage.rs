use std::path::{Path, PathBuf};

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

    pub fn is_onnx_installed(&self, model_id: &str) -> bool {
        let dir = self.onnx_dir_for(model_id);
        dir.is_dir() && dir.join("tokens.txt").is_file()
    }

    /// Remove an installed whisper model file. Removing a model that is not
    /// installed is a no-op.
    pub fn remove_model(&self, model_id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path_for(model_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Remove an installed ONNX model directory. Removing a model that is not
    /// installed is a no-op.
    pub fn remove_onnx(&self, model_id: &str) -> std::io::Result<()> {
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
        assert_eq!(
            storage.installed_models(),
            vec!["ggml-base.en".to_string()]
        );
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
        storage.remove_onnx("vits-piper-en-us-lessac-medium").unwrap();
        assert!(!storage.is_onnx_installed("vits-piper-en-us-lessac-medium"));
        assert!(!model_dir.exists());
        // removing again is a no-op
        storage.remove_onnx("vits-piper-en-us-lessac-medium").unwrap();
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
