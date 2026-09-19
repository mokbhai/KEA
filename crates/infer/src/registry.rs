use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::InferError;

/// The model families the app can install.
///
/// The variant names are also the wire strings: the UI already sends
/// "whisper" / "parakeet" / "tts" across IPC, so the serde representation and
/// [`ModelKind::as_str`] both spell them exactly that way. Parsing the string
/// once at a command boundary is what keeps every downstream match
/// exhaustive — and keeps a typo from producing a download key that no cancel
/// will ever match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Whisper,
    Parakeet,
    Tts,
}

impl ModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKind::Whisper => "whisper",
            ModelKind::Parakeet => "parakeet",
            ModelKind::Tts => "tts",
        }
    }

    /// The capability slot a model of this kind is bound to by default.
    pub fn default_slot(self) -> &'static str {
        match self {
            ModelKind::Whisper | ModelKind::Parakeet => "stt",
            ModelKind::Tts => "tts",
        }
    }

    /// The ONNX family tag stamped into bundle entries, or `None` for whisper,
    /// which is a single ggml file rather than an ONNX bundle.
    pub fn onnx_kind(self) -> Option<OnnxModelKind> {
        match self {
            ModelKind::Whisper => None,
            ModelKind::Parakeet => Some(OnnxModelKind::Parakeet),
            ModelKind::Tts => Some(OnnxModelKind::TtsVits),
        }
    }

    /// Identifies a download in flight. Start and cancel must derive the same
    /// key from the same (kind, model) pair or a cancel silently misses, which
    /// is why the key is derived here from the parsed kind and nowhere else.
    pub fn download_key(self, model_id: &str) -> String {
        match self {
            ModelKind::Whisper => format!("whisper:{model_id}"),
            other => format!("onnx:{}:{model_id}", other.as_str()),
        }
    }
}

impl FromStr for ModelKind {
    type Err = InferError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "whisper" => Ok(ModelKind::Whisper),
            "parakeet" => Ok(ModelKind::Parakeet),
            "tts" => Ok(ModelKind::Tts),
            other => Err(InferError::UnknownModelKind(other.to_string())),
        }
    }
}

impl TryFrom<&str> for ModelKind {
    type Error = InferError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl std::fmt::Display for ModelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One catalog entry, whichever family it belongs to. The metadata needed to
/// fetch and verify a model is the same for all three, so the catalog is
/// written once in this shape; [`ModelEntry::into_whisper`] and
/// [`ModelEntry::into_onnx`] produce the two historical entry types the IPC
/// layer still returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    /// UI-facing language tag, e.g. "en-US" / "en-GB" / "multilingual".
    pub language: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub kind: ModelKind,
}

impl ModelEntry {
    pub fn into_whisper(self) -> WhisperModelEntry {
        WhisperModelEntry {
            id: self.id,
            display_name: self.display_name,
            language: self.language,
            url: self.url,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
        }
    }

    /// `None` for whisper, which has no ONNX bundle shape.
    pub fn into_onnx(self) -> Option<OnnxModelEntry> {
        let kind = self.kind.onnx_kind()?;
        Some(OnnxModelEntry {
            id: self.id,
            display_name: self.display_name,
            language: self.language,
            url: self.url,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
            kind,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnnxModelKind {
    Parakeet,
    TtsVits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnnxModelEntry {
    pub id: String,
    pub display_name: String,
    /// UI-facing language tag, e.g. "en-US" / "en-GB" / "multilingual".
    pub language: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub kind: OnnxModelKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhisperModelEntry {
    pub id: String,
    pub display_name: String,
    /// UI-facing language tag, e.g. "en-US" / "en-GB" / "multilingual".
    pub language: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

pub struct ModelRegistry;

impl ModelRegistry {
    /// The catalog for one model family. This is the accessor that dispatches
    /// on kind; the per-kind ones below are shape-preserving views of it for
    /// the IPC layer, which still returns the two historical entry types.
    pub fn catalog(kind: ModelKind) -> Vec<ModelEntry> {
        match kind {
            ModelKind::Whisper => Self::whisper_entries(),
            ModelKind::Parakeet => Self::parakeet_entries(),
            ModelKind::Tts => Self::tts_entries(),
        }
    }

    /// Looks an id up in one family's catalog. Adding a family means adding an
    /// arm to [`ModelKind`], not another `find_*`.
    pub fn find(kind: ModelKind, id: &str) -> Option<ModelEntry> {
        Self::catalog(kind).into_iter().find(|entry| entry.id == id)
    }

    /// ONNX-shaped view of a family's catalog, or `None` for whisper, which
    /// ships a single ggml file rather than a bundle.
    pub fn onnx_catalog(kind: ModelKind) -> Option<Vec<OnnxModelEntry>> {
        kind.onnx_kind()?;
        Some(
            Self::catalog(kind)
                .into_iter()
                .filter_map(ModelEntry::into_onnx)
                .collect(),
        )
    }

    pub fn whisper_catalog() -> Vec<WhisperModelEntry> {
        Self::catalog(ModelKind::Whisper)
            .into_iter()
            .map(ModelEntry::into_whisper)
            .collect()
    }

    pub fn find_whisper(id: &str) -> Option<WhisperModelEntry> {
        Self::find(ModelKind::Whisper, id).map(ModelEntry::into_whisper)
    }

    pub fn parakeet_catalog() -> Vec<OnnxModelEntry> {
        Self::onnx_catalog(ModelKind::Parakeet).unwrap_or_default()
    }

    pub fn find_parakeet(id: &str) -> Option<OnnxModelEntry> {
        Self::find(ModelKind::Parakeet, id).and_then(ModelEntry::into_onnx)
    }

    pub fn tts_catalog() -> Vec<OnnxModelEntry> {
        Self::onnx_catalog(ModelKind::Tts).unwrap_or_default()
    }

    pub fn find_tts(id: &str) -> Option<OnnxModelEntry> {
        Self::find(ModelKind::Tts, id).and_then(ModelEntry::into_onnx)
    }

    fn whisper_entries() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                id: "ggml-base.en".into(),
                display_name: "Whisper Base (English)".into(),
                language: "en-US".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
                    .into(),
                size_bytes: 147_964_211,
                sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002".into(),
                kind: ModelKind::Whisper,
            },
            ModelEntry {
                id: "ggml-small.en".into(),
                display_name: "Whisper Small (English)".into(),
                language: "en-US".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
                    .into(),
                size_bytes: 487_614_201,
                sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d".into(),
                kind: ModelKind::Whisper,
            },
            ModelEntry {
                id: "ggml-medium.en".into(),
                display_name: "Whisper Medium (English)".into(),
                language: "en-US".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin"
                    .into(),
                size_bytes: 1_533_774_781,
                sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356".into(),
                kind: ModelKind::Whisper,
            },
        ]
    }

    fn parakeet_entries() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                id: "parakeet-tdt-0.6b-v2".into(),
                display_name: "Parakeet TDT v2 (English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2"
                    .into(),
                size_bytes: 482_468_385,
                sha256: "157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad"
                    .into(),
                kind: ModelKind::Parakeet,
            },
            ModelEntry {
                id: "parakeet-tdt-0.6b-v3".into(),
                display_name: "Parakeet TDT v3 (25 languages)".into(),
                language: "multilingual".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"
                    .into(),
                size_bytes: 487_170_055,
                sha256: "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf"
                    .into(),
                kind: ModelKind::Parakeet,
            },
        ]
    }

    fn tts_entries() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                id: "vits-piper-en-us-lessac-medium".into(),
                display_name: "Piper Lessac Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-lessac-medium.tar.bz2"
                    .into(),
                size_bytes: 67_230_653,
                sha256: "9e3febfacf0abf4270172d2958bcec246032b7e88efc2720840cc80c93de334e"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-amy-low".into(),
                display_name: "Piper Amy Low (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-amy-low.tar.bz2"
                    .into(),
                size_bytes: 67_095_344,
                sha256: "c70f5284a09a7fd4ed203b39b2ff51cac1432b422b852eb647b481dade3cf639"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-amy-medium".into(),
                display_name: "Piper Amy Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-amy-medium.tar.bz2"
                    .into(),
                size_bytes: 67_223_746,
                sha256: "9a5d1fc497f85e8022b785bff5f8105203b1e33099ee6265203efc70b0cb0264"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-ryan-high".into(),
                display_name: "Piper Ryan High (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-ryan-high.tar.bz2"
                    .into(),
                size_bytes: 115_630_708,
                sha256: "6a71edf4d308b9cb2eaeadc8d1f3c6bf96120ecb7fe52c29a2b6e139c59760ed"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-ryan-medium".into(),
                display_name: "Piper Ryan Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-ryan-medium.tar.bz2"
                    .into(),
                size_bytes: 67_213_100,
                sha256: "c546af78b6395b4e7c4ce1ed899438b64426a362f5d4ec5fecd090ded9ad7505"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-hfc-female-medium".into(),
                display_name: "Piper HFC Female Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-hfc_female-medium.tar.bz2"
                    .into(),
                size_bytes: 67_228_166,
                sha256: "3fffdceb0c65bd9415a085d09c3cb88cc82f9d74a6ca453f8ce7fc5eaee81ff8"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-hfc-male-medium".into(),
                display_name: "Piper HFC Male Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-hfc_male-medium.tar.bz2"
                    .into(),
                size_bytes: 67_214_049,
                sha256: "76388f84acfca8ba5c0ed1636a26ada14c598abd52e76f110d4756fe326fc5f2"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-us-libritts-r-medium".into(),
                display_name: "Piper LibriTTS-R Medium (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-libritts_r-medium.tar.bz2"
                    .into(),
                size_bytes: 82_038_311,
                sha256: "10dc268f3e371696d721486123e2705a9fc1faa113491979fde4d88dba1f1b1c"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-gb-alan-medium".into(),
                display_name: "Piper Alan Medium (British English)".into(),
                language: "en-GB".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_GB-alan-medium.tar.bz2"
                    .into(),
                size_bytes: 67_220_121,
                sha256: "a48d4017da0f77668b27bed63fe6e04dd64c6397e1fadad4f460efb0ef7c9012"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-gb-cori-high".into(),
                display_name: "Piper Cori High (British English)".into(),
                language: "en-GB".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_GB-cori-high.tar.bz2"
                    .into(),
                size_bytes: 115_574_061,
                sha256: "42922f07738fcde2e49eed4e959635692f73b933de35a6b7c1010162ff566292"
                    .into(),
                kind: ModelKind::Tts,
            },
            ModelEntry {
                id: "vits-piper-en-gb-jenny-dioco-medium".into(),
                display_name: "Piper Jenny Dioco Medium (British English)".into(),
                language: "en-GB".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_GB-jenny_dioco-medium.tar.bz2"
                    .into(),
                size_bytes: 67_225_842,
                sha256: "a0888024569bafbefc05a4b48ddf8419d8dbbf3205f4af37cf7c6f1a87cc20c5"
                    .into(),
                kind: ModelKind::Tts,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_includes_base_en() {
        let models = ModelRegistry::whisper_catalog();
        assert!(models.iter().any(|m| m.id == "ggml-base.en"));
        let found = ModelRegistry::find_whisper("ggml-base.en").unwrap();
        assert!(found.url.contains("ggml-base.en"));
        assert!(!found.sha256.is_empty());
        assert!(found.size_bytes > 0);
    }

    #[test]
    fn catalog_has_base_small_medium() {
        let models = ModelRegistry::whisper_catalog();
        assert_eq!(models.len(), 3);
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"ggml-base.en"));
        assert!(ids.contains(&"ggml-small.en"));
        assert!(ids.contains(&"ggml-medium.en"));
    }

    #[test]
    fn find_whisper_returns_none_for_unknown() {
        assert!(ModelRegistry::find_whisper("nonexistent").is_none());
    }

    #[test]
    fn find_whisper_matches_catalog_entry() {
        let catalog = ModelRegistry::whisper_catalog();
        for entry in &catalog {
            assert_eq!(ModelRegistry::find_whisper(&entry.id), Some(entry.clone()));
        }
    }

    #[test]
    fn parakeet_catalog_has_entry() {
        let models = ModelRegistry::parakeet_catalog();
        assert!(!models.is_empty());
        assert!(models[0].url.starts_with("https://"));
        assert_eq!(models[0].kind, OnnxModelKind::Parakeet);
    }

    #[test]
    fn tts_catalog_has_entry() {
        let models = ModelRegistry::tts_catalog();
        assert!(!models.is_empty());
        assert!(models[0].url.starts_with("https://"));
        assert_eq!(models[0].kind, OnnxModelKind::TtsVits);
    }

    #[test]
    fn tts_catalog_entries_are_valid() {
        let models = ModelRegistry::tts_catalog();
        assert_eq!(models.len(), 11);
        // The sherpa-tts engine uses the first entry as its default model —
        // keep lessac first so reordering can't silently change the default voice.
        assert_eq!(models[0].id, "vits-piper-en-us-lessac-medium");
        let mut ids = std::collections::HashSet::new();
        for entry in &models {
            assert!(ids.insert(entry.id.clone()), "duplicate id: {}", entry.id);
            assert!(entry.url.starts_with("https://"), "bad url: {}", entry.url);
            assert_eq!(entry.sha256.len(), 64, "bad sha256 length: {}", entry.id);
            assert!(
                entry
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "sha256 not lowercase hex: {}",
                entry.id
            );
            assert!(entry.size_bytes > 0, "zero size: {}", entry.id);
            assert_eq!(entry.kind, OnnxModelKind::TtsVits);
            assert!(
                !entry.display_name.is_empty(),
                "empty display_name: {}",
                entry.id
            );
            // Language must match the locale baked into the model id.
            let expected = if entry.id.contains("en-gb") {
                "en-GB"
            } else {
                "en-US"
            };
            assert_eq!(entry.language, expected, "wrong language: {}", entry.id);
        }
    }

    #[test]
    fn whisper_and_parakeet_entries_have_display_name_and_language() {
        for entry in ModelRegistry::whisper_catalog() {
            assert!(
                !entry.display_name.is_empty(),
                "empty display_name: {}",
                entry.id
            );
            assert_eq!(entry.language, "en-US", "wrong language: {}", entry.id);
        }
        for entry in ModelRegistry::parakeet_catalog() {
            assert!(
                !entry.display_name.is_empty(),
                "empty display_name: {}",
                entry.id
            );
            assert!(!entry.language.is_empty(), "empty language: {}", entry.id);
        }
        // v3 supports 25 languages; it must not claim a single locale.
        let v3 = ModelRegistry::find_parakeet("parakeet-tdt-0.6b-v3").unwrap();
        assert_eq!(v3.language, "multilingual");
    }

    #[test]
    fn model_kind_round_trips_through_its_wire_string() {
        for kind in [ModelKind::Whisper, ModelKind::Parakeet, ModelKind::Tts] {
            assert_eq!(ModelKind::try_from(kind.as_str()).unwrap(), kind);
            // The serde form is the same string the UI already sends.
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
    }

    #[test]
    fn unknown_model_kind_is_rejected_in_one_place() {
        let err = "bogus".parse::<ModelKind>().unwrap_err();
        assert!(matches!(err, InferError::UnknownModelKind(ref k) if k == "bogus"));
        assert!(err.to_string().contains("unknown model kind: bogus"));
    }

    /// The download key is the cancel contract: a kind that failed open used to
    /// yield "onnx:bogus:id", a key no cancel could ever match.
    #[test]
    fn download_key_matches_the_shipped_key_shape() {
        assert_eq!(
            ModelKind::Whisper.download_key("ggml-base.en"),
            "whisper:ggml-base.en"
        );
        assert_eq!(
            ModelKind::Parakeet.download_key("parakeet-tdt-0.6b-v2"),
            "onnx:parakeet:parakeet-tdt-0.6b-v2"
        );
        assert_eq!(ModelKind::Tts.download_key("v"), "onnx:tts:v");
    }

    #[test]
    fn default_slot_per_kind() {
        assert_eq!(ModelKind::Whisper.default_slot(), "stt");
        assert_eq!(ModelKind::Parakeet.default_slot(), "stt");
        assert_eq!(ModelKind::Tts.default_slot(), "tts");
    }

    #[test]
    fn dispatching_accessor_agrees_with_the_per_kind_views() {
        assert_eq!(
            ModelRegistry::catalog(ModelKind::Whisper).len(),
            ModelRegistry::whisper_catalog().len()
        );
        assert_eq!(
            ModelRegistry::onnx_catalog(ModelKind::Parakeet).unwrap(),
            ModelRegistry::parakeet_catalog()
        );
        assert_eq!(
            ModelRegistry::onnx_catalog(ModelKind::Tts).unwrap(),
            ModelRegistry::tts_catalog()
        );
        // Whisper has no bundle shape, so the ONNX view refuses rather than
        // quietly handing back an empty catalog.
        assert!(ModelRegistry::onnx_catalog(ModelKind::Whisper).is_none());

        for kind in [ModelKind::Whisper, ModelKind::Parakeet, ModelKind::Tts] {
            for entry in ModelRegistry::catalog(kind) {
                assert_eq!(entry.kind, kind);
                assert_eq!(ModelRegistry::find(kind, &entry.id), Some(entry));
            }
            assert!(ModelRegistry::find(kind, "nonexistent").is_none());
        }
    }

    #[test]
    fn find_parakeet_and_tts_match_catalog() {
        for entry in ModelRegistry::parakeet_catalog() {
            assert_eq!(ModelRegistry::find_parakeet(&entry.id), Some(entry));
        }
        for entry in ModelRegistry::tts_catalog() {
            assert_eq!(ModelRegistry::find_tts(&entry.id), Some(entry));
        }
    }
}
