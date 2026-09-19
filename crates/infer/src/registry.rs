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
    /// The bundle shape, when the family alone no longer says it.
    ///
    /// `ModelKind::Tts` used to imply one ONNX layout, so
    /// [`ModelKind::onnx_kind`] could derive it. Kokoro and Kitten are the
    /// same family, the same storage root and the same picker section but
    /// three different sherpa model configs, so the shape is authored here
    /// when it differs from the family default and derived otherwise —
    /// rather than splitting the family and with it the IPC kind string, the
    /// storage root and the download-key namespace.
    #[serde(default)]
    pub onnx_kind: Option<OnnxModelKind>,
    /// Still resolvable, no longer offered.
    ///
    /// Dropping a catalog row outright is not retirement: `find` is what
    /// `validate_model_id_for_delete` consults, so a deleted row leaves
    /// whoever already downloaded the model unable to remove it from the UI.
    /// A deprecated entry keeps resolving — for delete, for an existing
    /// binding — and the picker hides it unless it is installed.
    #[serde(default)]
    pub deprecated: bool,
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
            deprecated: self.deprecated,
        }
    }

    /// `None` for whisper, which has no ONNX bundle shape.
    pub fn into_onnx(self) -> Option<OnnxModelEntry> {
        // The authored shape wins; the family default covers every entry that
        // does not need to say.
        let kind = self.onnx_kind.or_else(|| self.kind.onnx_kind())?;
        Some(OnnxModelEntry {
            id: self.id,
            display_name: self.display_name,
            language: self.language,
            url: self.url,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
            kind,
            deprecated: self.deprecated,
        })
    }
}

/// The sherpa-onnx model config a bundle has to be loaded through.
///
/// Not a taxonomy of vendors: it is exactly the set of `OfflineTtsModelConfig`
/// arms (plus the transducer) the app knows how to fill, which is why "which
/// files does the bundle hold" and "which config gets them" are the same
/// question and answered in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnnxModelKind {
    Parakeet,
    TtsVits,
    TtsKokoro,
    TtsKitten,
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
    /// See [`ModelEntry::deprecated`].
    #[serde(default)]
    pub deprecated: bool,
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
    /// See [`ModelEntry::deprecated`].
    #[serde(default)]
    pub deprecated: bool,
}

/// One selectable speaker inside a multi-speaker ONNX voice bundle.
///
/// Kokoro and Kitten ship every speaker in a single `voices.bin` and address
/// them by integer `sid` (sherpa's `GenerationConfig::sid`). The stored
/// setting stays the *name*, so a picker reads "af_bella" rather than "1" and
/// a bundle that reorders its table cannot silently change the user's voice
/// into a different one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnnxVoice {
    pub sid: i32,
    pub name: String,
    /// BCP-47 tag for this one speaker, which in a multilingual bundle is not
    /// the model's own language.
    pub language: String,
}

/// The speaker table for one model id. `sid` is the index, because that is
/// what the bundle's `voices.bin` uses.
struct VoiceTable {
    model_id: &'static str,
    /// `(name, language)` in sid order.
    speakers: &'static [(&'static str, &'static str)],
}

/// Speaker tables for the multi-speaker bundles, in `sid` order.
///
/// Deliberately not derived from the bundle on disk: `voices.bin` is a raw
/// embedding table with no names in it, so the names have to come from the
/// model card. A model with no table here is not an error — synthesis falls
/// back to sid 0 (see [`ModelRegistry::voice_sid`]) and the picker offers only
/// the default, which is the honest thing to show for a mapping nobody has
/// verified.
static VOICE_TABLES: &[VoiceTable] = &[
    VoiceTable {
        model_id: "kokoro-en-v0.19",
        speakers: &[
            ("af", "en-US"),
            ("af_bella", "en-US"),
            ("af_nicole", "en-US"),
            ("af_sarah", "en-US"),
            ("af_sky", "en-US"),
            ("am_adam", "en-US"),
            ("am_michael", "en-US"),
            ("bf_emma", "en-GB"),
            ("bf_isabella", "en-GB"),
            ("bm_george", "en-GB"),
            ("bm_lewis", "en-GB"),
        ],
    },
    VoiceTable {
        model_id: "kitten-nano-en-v0.2",
        speakers: &[
            ("expr-voice-2-m", "en-US"),
            ("expr-voice-2-f", "en-US"),
            ("expr-voice-3-m", "en-US"),
            ("expr-voice-3-f", "en-US"),
            ("expr-voice-4-m", "en-US"),
            ("expr-voice-4-f", "en-US"),
            ("expr-voice-5-m", "en-US"),
            ("expr-voice-5-f", "en-US"),
        ],
    },
    // kokoro-multi-lang-v1.1 ships 100+ speakers and no published sid order
    // that could be checked without the bundle in hand. Guessing it would
    // hand the user a different voice than the one they picked, which is
    // worse than offering only the default, so it has no table until the
    // order is measured.
];

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

    /// The catalog a picker should offer: everything that is not retired.
    ///
    /// `catalog`/`find` stay complete on purpose — delete validation and an
    /// existing binding both have to keep resolving a retired id.
    pub fn offered(kind: ModelKind) -> Vec<ModelEntry> {
        Self::catalog(kind)
            .into_iter()
            .filter(|entry| !entry.deprecated)
            .collect()
    }

    /// The selectable speakers in a model's bundle, empty for a single-speaker
    /// one (every Piper voice) or one whose table is not known.
    pub fn voices(model_id: &str) -> Vec<OnnxVoice> {
        VOICE_TABLES
            .iter()
            .find(|table| table.model_id == model_id)
            .map(|table| {
                table
                    .speakers
                    .iter()
                    .enumerate()
                    .map(|(sid, (name, language))| OnnxVoice {
                        sid: sid as i32,
                        name: (*name).to_string(),
                        language: (*language).to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves a stored voice *name* to the `sid` the bundle addresses it by.
    ///
    /// A name that is not in the table is not an error: switching models keeps
    /// the old name in settings, and refusing to speak because of it would be
    /// a worse outcome than speaking in the default voice. Warn and fall back
    /// to 0, which every bundle has.
    pub fn voice_sid(model_id: &str, voice: Option<&str>) -> i32 {
        let Some(name) = voice.map(str::trim).filter(|v| !v.is_empty()) else {
            return 0;
        };
        match Self::voices(model_id)
            .into_iter()
            .find(|candidate| candidate.name == name)
        {
            Some(found) => found.sid,
            None => {
                tracing::warn!(
                    model = %model_id,
                    voice = %name,
                    "voice is not in this model's speaker table; using the default voice"
                );
                0
            }
        }
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
            },
            ModelEntry {
                id: "ggml-large-v3-turbo-q5_0".into(),
                display_name: "Whisper Large v3 Turbo (Q5, multilingual)".into(),
                language: "multilingual".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin"
                    .into(),
                size_bytes: 574_041_195,
                sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2".into(),
                kind: ModelKind::Whisper,
                onnx_kind: None,
                deprecated: false,
            },
            ModelEntry {
                id: "ggml-large-v3-turbo-q8_0".into(),
                display_name: "Whisper Large v3 Turbo (Q8, multilingual)".into(),
                language: "multilingual".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin"
                    .into(),
                size_bytes: 874_188_075,
                sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1".into(),
                kind: ModelKind::Whisper,
                onnx_kind: None,
                deprecated: false,
            },
            // Retired, not removed: 1.5 GB for a worse result than the 574 MB
            // turbo above is a bug in the catalog, but the row has to stay so
            // anyone who already downloaded it can still delete it — see
            // `deprecated`.
            ModelEntry {
                id: "ggml-medium.en".into(),
                display_name: "Whisper Medium (English)".into(),
                language: "en-US".into(),
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin"
                    .into(),
                size_bytes: 1_533_774_781,
                sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356".into(),
                kind: ModelKind::Whisper,
                onnx_kind: None,
                deprecated: true,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
            },
        ]
    }

    fn tts_entries() -> Vec<ModelEntry> {
        vec![
            // Kitten first, because the engine offers the first *installed*
            // entry as its fallback and a 26 MB download is not a decision a
            // new user should have to make. The Piper voices below stay for
            // anyone already bound to one.
            ModelEntry {
                id: "kitten-nano-en-v0.2".into(),
                display_name: "Kitten Nano v0.2 (US English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kitten-nano-en-v0_2-fp16.tar.bz2"
                    .into(),
                size_bytes: 26_586_708,
                sha256: "0345a8a2f4a710cb8f7912c9a731ded8b3e1e69b33a871efa95c2e64651518fe"
                    .into(),
                kind: ModelKind::Tts,
                onnx_kind: Some(OnnxModelKind::TtsKitten),
                deprecated: false,
            },
            // The int8 builds, not the fp32 ones: 103 MB against 320 MB and
            // 147 MB against 365 MB, for a voice difference nobody reports
            // hearing. A quarter of a gigabyte is a real cost to a user.
            ModelEntry {
                id: "kokoro-en-v0.19".into(),
                display_name: "Kokoro v0.19 (US/UK English)".into(),
                language: "en-US".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-en-v0_19.tar.bz2"
                    .into(),
                size_bytes: 103_248_205,
                sha256: "c9f0dd393615805b0bab050c340834d5e684e732aec91c0e860cd30e982c08bd"
                    .into(),
                kind: ModelKind::Tts,
                onnx_kind: Some(OnnxModelKind::TtsKokoro),
                deprecated: false,
            },
            ModelEntry {
                id: "kokoro-multi-lang-v1.1".into(),
                display_name: "Kokoro v1.1 (multilingual)".into(),
                language: "multilingual".into(),
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-multi-lang-v1_1.tar.bz2"
                    .into(),
                size_bytes: 147_031_220,
                sha256: "a1e94694776049035c4f2c6529f003aaece993c76aae9a78995831c3c4dcafc6"
                    .into(),
                kind: ModelKind::Tts,
                onnx_kind: Some(OnnxModelKind::TtsKokoro),
                deprecated: false,
            },
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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
                onnx_kind: None,
                deprecated: false,
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

    /// The whisper catalog, spelled out: this is the test that catches a
    /// retirement done by deleting the row instead of flagging it — which
    /// would leave an already-downloaded model undeletable from the UI.
    #[test]
    fn whisper_catalog_lists_every_id_including_the_retired_one() {
        let models = ModelRegistry::whisper_catalog();
        assert_eq!(models.len(), 5);
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "ggml-base.en",
                "ggml-small.en",
                "ggml-large-v3-turbo-q5_0",
                "ggml-large-v3-turbo-q8_0",
                "ggml-medium.en",
            ]
        );
    }

    #[test]
    fn large_v3_turbo_resolves_and_is_multilingual() {
        for id in ["ggml-large-v3-turbo-q5_0", "ggml-large-v3-turbo-q8_0"] {
            let found = ModelRegistry::find_whisper(id).expect("turbo entry");
            assert!(
                found.url.ends_with(&format!("{id}.bin")),
                "bad url for {id}"
            );
            assert_eq!(found.language, "multilingual");
            assert!(!found.deprecated);
        }
    }

    /// The picker reads the catalog top to bottom, so "bigger means better"
    /// only reads sensibly if the list is ordered by size.
    #[test]
    fn catalogs_are_ordered_by_ascending_size() {
        for kind in [ModelKind::Whisper, ModelKind::Parakeet] {
            let sizes: Vec<u64> = ModelRegistry::catalog(kind)
                .iter()
                .map(|entry| entry.size_bytes)
                .collect();
            assert!(
                sizes.windows(2).all(|w| w[0] <= w[1]),
                "{kind} catalog is not ordered by size: {sizes:?}"
            );
        }
    }

    /// medium.en is retired, not deleted: the picker must stop offering it
    /// while `find` — which `validate_model_id_for_delete` consults — keeps
    /// resolving it, or 1.5 GB becomes unremovable from the UI.
    #[test]
    fn medium_en_is_retired_but_still_resolvable() {
        let medium = ModelRegistry::find_whisper("ggml-medium.en").expect("still in the catalog");
        assert!(medium.deprecated);
        assert!(ModelRegistry::find(ModelKind::Whisper, "ggml-medium.en").is_some());

        let offered: Vec<_> = ModelRegistry::offered(ModelKind::Whisper)
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert!(!offered.contains(&"ggml-medium.en".to_string()));
        assert!(offered.contains(&"ggml-large-v3-turbo-q5_0".to_string()));
        // Nothing else is retired, in any family.
        for kind in [ModelKind::Whisper, ModelKind::Parakeet, ModelKind::Tts] {
            assert_eq!(
                ModelRegistry::catalog(kind).len() - ModelRegistry::offered(kind).len(),
                usize::from(kind == ModelKind::Whisper),
                "unexpected retirement in the {kind} catalog"
            );
        }
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
        // The head of the TTS catalog is the default voice a fresh install
        // gets, and it is a Kitten bundle now — see `tts_catalog_entries_are_valid`.
        assert_eq!(models[0].kind, OnnxModelKind::TtsKitten);
    }

    /// One gate for all three catalogs. Every field on `ModelEntry` is load
    /// bearing — a wrong sha256 fails the download at the verify step, a
    /// wrong size only shows as a progress bar that lies, a duplicate id
    /// makes `find` ambiguous — so they are checked once here rather than
    /// per family, where the whisper catalog used to check only two of them.
    fn assert_catalog_is_valid(kind: ModelKind) {
        let models = ModelRegistry::catalog(kind);
        assert!(!models.is_empty(), "{kind} catalog is empty");
        let mut ids = std::collections::HashSet::new();
        for entry in &models {
            assert_eq!(entry.kind, kind, "entry in the wrong catalog: {}", entry.id);
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
            assert!(
                !entry.display_name.is_empty(),
                "empty display_name: {}",
                entry.id
            );
            assert!(!entry.language.is_empty(), "empty language: {}", entry.id);
        }
    }

    #[test]
    fn every_catalog_entry_is_complete() {
        for kind in [ModelKind::Whisper, ModelKind::Parakeet, ModelKind::Tts] {
            assert_catalog_is_valid(kind);
        }
    }

    #[test]
    fn tts_catalog_entries_are_valid() {
        let models = ModelRegistry::tts_catalog();
        assert_eq!(models.len(), 14);
        // The sherpa-tts engine falls back to the first entry it finds
        // installed, so the head of this list is the default voice a fresh
        // install gets — 26 MB rather than 67 MB, and a better voice.
        assert_eq!(models[0].id, "kitten-nano-en-v0.2");
        for entry in &models {
            // Per-entry kind, not a constant: the TTS family now holds three
            // bundle shapes, and asserting a constant here is exactly what
            // would hide a Kokoro entry being loaded as VITS.
            let expected = match entry.id.as_str() {
                id if id.starts_with("kokoro-") => OnnxModelKind::TtsKokoro,
                id if id.starts_with("kitten-") => OnnxModelKind::TtsKitten,
                _ => OnnxModelKind::TtsVits,
            };
            assert_eq!(entry.kind, expected, "wrong bundle shape: {}", entry.id);
        }
        // The Piper voices name their locale in their id; the rest say so
        // in the entry.
        for entry in models.iter().filter(|e| e.id.starts_with("vits-piper-")) {
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
        // v3 supports 25 languages; it must not claim a single locale.
        let v3 = ModelRegistry::find_parakeet("parakeet-tdt-0.6b-v3").unwrap();
        assert_eq!(v3.language, "multilingual");
        let base = ModelRegistry::find_whisper("ggml-base.en").unwrap();
        assert_eq!(base.language, "en-US");
    }

    #[test]
    fn kokoro_voice_names_resolve_to_their_speaker_ids() {
        let voices = ModelRegistry::voices("kokoro-en-v0.19");
        assert_eq!(voices.len(), 11);
        assert_eq!(voices[0].sid, 0);
        assert_eq!(voices[0].name, "af");
        // sid is the index, and the table is what the picker shows.
        for (i, voice) in voices.iter().enumerate() {
            assert_eq!(voice.sid, i as i32);
            assert_eq!(
                ModelRegistry::voice_sid("kokoro-en-v0.19", Some(&voice.name)),
                voice.sid
            );
        }
        assert_eq!(
            ModelRegistry::voice_sid("kokoro-en-v0.19", Some("bm_lewis")),
            10
        );
    }

    #[test]
    fn kitten_ships_its_own_speaker_table() {
        let voices = ModelRegistry::voices("kitten-nano-en-v0.2");
        assert_eq!(voices.len(), 8);
        assert_eq!(voices[0].name, "expr-voice-2-m");
        assert_eq!(
            ModelRegistry::voice_sid("kitten-nano-en-v0.2", Some("expr-voice-5-f")),
            7
        );
    }

    /// Swapping models leaves the old voice name in settings. Refusing to
    /// speak over that would be a worse outcome than speaking in the default
    /// voice, so an unknown name resolves to sid 0 rather than erroring.
    #[test]
    fn an_unknown_voice_falls_back_to_the_default_speaker() {
        assert_eq!(
            ModelRegistry::voice_sid("kokoro-en-v0.19", Some("alloy")),
            0
        );
        assert_eq!(ModelRegistry::voice_sid("kokoro-en-v0.19", None), 0);
        assert_eq!(ModelRegistry::voice_sid("kokoro-en-v0.19", Some("  ")), 0);
        // A single-speaker Piper voice has no table at all.
        assert!(ModelRegistry::voices("vits-piper-en-us-lessac-medium").is_empty());
        assert_eq!(
            ModelRegistry::voice_sid("vits-piper-en-us-lessac-medium", Some("af")),
            0
        );
    }

    /// A voice table that names a model the catalog does not have is dead
    /// data the picker would never show.
    #[test]
    fn every_voice_table_belongs_to_a_catalogued_model() {
        let ids: std::collections::HashSet<String> = ModelRegistry::tts_catalog()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        for table in VOICE_TABLES {
            assert!(
                ids.contains(table.model_id),
                "voice table for an unknown model: {}",
                table.model_id
            );
            let mut names = std::collections::HashSet::new();
            for (name, _) in table.speakers {
                assert!(
                    names.insert(*name),
                    "duplicate speaker {name} in {}",
                    table.model_id
                );
            }
        }
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
