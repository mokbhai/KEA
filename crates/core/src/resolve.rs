use kea_engines::EngineRegistry;
use crate::error::KeaError;
use crate::store::bindings::{Binding, BindingRepo};

/// Reserved feature id for capability-wide default bindings: a row stored as
/// `("default", "llm"|"stt"|"tts")` applies to every feature without an
/// explicit binding of its own.
pub const DEFAULT_FEATURE_ID: &str = "default";

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The winning binding: the feature's own row, the capability default, or
    /// an auto-bound engine (model/provider_ref None). Carrying the whole
    /// binding lets consumers use a default's model/provider_ref without
    /// re-reading the feature row (which would drop them).
    Bound(Binding),
    NeedsChoice(Vec<String>), // candidate engine ids
    Unresolvable,             // bound to a missing engine, or no candidates
}

fn auto_binding(engine_id: String) -> Binding {
    Binding {
        engine_id,
        model: None,
        provider_ref: None,
    }
}

pub struct SlotResolver<'a> {
    engines: &'a EngineRegistry,
    bindings: &'a BindingRepo,
}

impl<'a> SlotResolver<'a> {
    pub fn new(engines: &'a EngineRegistry, bindings: &'a BindingRepo) -> Self {
        Self { engines, bindings }
    }

    pub async fn resolve_llm(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError> {
        if let Some(b) = self.bindings.get(feature_id, slot).await? {
            return Ok(if self.engines.llm(&b.engine_id).is_some() {
                Resolution::Bound(b)
            } else {
                Resolution::Unresolvable
            });
        }
        // Capability default. Unlike a feature binding, a default naming a
        // missing engine falls through to the auto/unresolvable path.
        if let Some(b) = self.bindings.get(DEFAULT_FEATURE_ID, "llm").await? {
            if self.engines.llm(&b.engine_id).is_some() {
                return Ok(Resolution::Bound(b));
            }
        }
        let candidates = self.engines.list_llm_ids();
        Ok(match candidates.len() {
            0 => Resolution::Unresolvable,
            1 => Resolution::Bound(auto_binding(candidates.into_iter().next().unwrap())),
            _ => Resolution::NeedsChoice(candidates),
        })
    }

    pub async fn resolve_stt(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError> {
        if let Some(b) = self.bindings.get(feature_id, slot).await? {
            return Ok(if self.engines.stt(&b.engine_id).is_some() {
                Resolution::Bound(b)
            } else {
                Resolution::Unresolvable
            });
        }
        if let Some(b) = self.bindings.get(DEFAULT_FEATURE_ID, "stt").await? {
            if self.engines.stt(&b.engine_id).is_some() {
                return Ok(Resolution::Bound(b));
            }
        }
        let candidates = self.engines.list_stt_ids();
        Ok(match candidates.len() {
            0 => Resolution::Unresolvable,
            1 => Resolution::Bound(auto_binding(candidates.into_iter().next().unwrap())),
            _ => Resolution::NeedsChoice(candidates),
        })
    }

    pub async fn resolve_tts(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError> {
        if let Some(b) = self.bindings.get(feature_id, slot).await? {
            return Ok(if self.engines.tts(&b.engine_id).is_some() {
                Resolution::Bound(b)
            } else {
                Resolution::Unresolvable
            });
        }
        if let Some(b) = self.bindings.get(DEFAULT_FEATURE_ID, "tts").await? {
            if self.engines.tts(&b.engine_id).is_some() {
                return Ok(Resolution::Bound(b));
            }
        }
        let candidates = self.engines.list_tts_ids();
        Ok(match candidates.len() {
            0 => Resolution::Unresolvable,
            1 => Resolution::Bound(auto_binding(candidates.into_iter().next().unwrap())),
            _ => Resolution::NeedsChoice(candidates),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};
    use crate::store::bindings::{Binding, BindingRepo};
    use kea_engines::{noop::NoopSttEngine, EngineRegistry, noop::NoopLlmEngine, noop::NoopTtsEngine};
    use std::sync::Arc;

    async fn setup() -> (BindingRepo, EngineRegistry) {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        (BindingRepo::new(pool), EngineRegistry::default())
    }

    #[tokio::test]
    async fn unbound_single_engine_autobinds() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop"));
    }

    #[tokio::test]
    async fn unbound_multiple_engines_needs_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(crate::resolve::tests::SecondNoop));
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::NeedsChoice(v) if v.len() == 2));
    }

    #[tokio::test]
    async fn bound_to_missing_engine_is_unresolvable() {
        let (bindings, reg) = setup().await;
        bindings.set("demo", "llm", Binding {
            engine_id: "ghost".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Unresolvable));
    }

    #[tokio::test]
    async fn unbound_single_stt_engine_autobinds() {
        let (bindings, mut reg) = setup().await;
        reg.register_stt(Arc::new(NoopSttEngine));
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_stt("dictation", "stt")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop-stt"));
    }

    #[tokio::test]
    async fn unbound_multiple_stt_engines_needs_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_stt(Arc::new(NoopSttEngine));
        reg.register_stt(Arc::new(SecondNoopStt));
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_stt("dictation", "stt")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::NeedsChoice(v) if v.len() == 2));
    }

    #[tokio::test]
    async fn bound_stt_to_missing_engine_is_unresolvable() {
        let (bindings, reg) = setup().await;
        bindings
            .set(
                "dictation",
                "stt",
                Binding {
                    engine_id: "ghost-stt".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_stt("dictation", "stt")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Unresolvable));
    }

    #[tokio::test]
    async fn unbound_single_tts_engine_autobinds() {
        let (bindings, mut reg) = setup().await;
        reg.register_tts(Arc::new(NoopTtsEngine));
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_tts("tts", "tts")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop-tts"));
    }

    #[tokio::test]
    async fn unbound_multiple_tts_engines_needs_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_tts(Arc::new(NoopTtsEngine));
        reg.register_tts(Arc::new(SecondNoopTts));
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_tts("tts", "tts")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::NeedsChoice(v) if v.len() == 2));
    }

    #[tokio::test]
    async fn bound_tts_to_missing_engine_is_unresolvable() {
        let (bindings, reg) = setup().await;
        bindings
            .set(
                "tts",
                "tts",
                Binding {
                    engine_id: "ghost-tts".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_tts("tts", "tts")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Unresolvable));
    }

    #[tokio::test]
    async fn feature_override_beats_capability_default() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(SecondNoop));
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "noop2".into(), model: None, provider_ref: None }).await.unwrap();
        bindings.set("demo", "llm", Binding {
            engine_id: "noop".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop"));
    }

    #[tokio::test]
    async fn capability_default_beats_auto_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(SecondNoop));
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "noop2".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop2"));
    }

    #[tokio::test]
    async fn default_to_missing_engine_falls_through_to_auto() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "ghost".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop"));
    }

    #[tokio::test]
    async fn default_to_missing_engine_is_unresolvable_without_candidates() {
        let (bindings, reg) = setup().await;
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "ghost".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Unresolvable));
    }

    #[tokio::test]
    async fn stt_capability_default_beats_auto_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_stt(Arc::new(NoopSttEngine));
        reg.register_stt(Arc::new(SecondNoopStt));
        bindings.set(DEFAULT_FEATURE_ID, "stt", Binding {
            engine_id: "noop-stt2".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_stt("dictation", "stt")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop-stt2"));
    }

    #[tokio::test]
    async fn tts_capability_default_beats_auto_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_tts(Arc::new(NoopTtsEngine));
        reg.register_tts(Arc::new(SecondNoopTts));
        bindings.set(DEFAULT_FEATURE_ID, "tts", Binding {
            engine_id: "noop-tts2".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings)
            .resolve_tts("tts", "tts")
            .await
            .unwrap();
        assert!(matches!(r, Resolution::Bound(b) if b.engine_id == "noop-tts2"));
    }

    #[tokio::test]
    async fn capability_default_carries_model_and_provider_ref() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(SecondNoop));
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "noop2".into(),
            model: Some("gpt-4o-mini".into()),
            provider_ref: Some("openai".into()),
        }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        let Resolution::Bound(b) = r else { panic!("expected Bound, got {r:?}") };
        assert_eq!(b.engine_id, "noop2");
        assert_eq!(b.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(b.provider_ref.as_deref(), Some("openai"));
    }

    #[tokio::test]
    async fn feature_override_model_wins_over_default_model() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(SecondNoop));
        bindings.set(DEFAULT_FEATURE_ID, "llm", Binding {
            engine_id: "noop2".into(),
            model: Some("default-model".into()),
            provider_ref: Some("default-provider".into()),
        }).await.unwrap();
        bindings.set("demo", "llm", Binding {
            engine_id: "noop".into(),
            model: Some("override-model".into()),
            provider_ref: None,
        }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        let Resolution::Bound(b) = r else { panic!("expected Bound, got {r:?}") };
        assert_eq!(b.engine_id, "noop");
        assert_eq!(b.model.as_deref(), Some("override-model"));
        assert_eq!(b.provider_ref, None);
    }

    // a second engine id for the multi-engine test
    use async_trait::async_trait;
    use kea_engines::{
        AudioPcm, EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, SttEngine, SttOpts,
        Transcript, TtsEngine, TtsOpts,
    };
    pub struct SecondNoop;
    #[async_trait]
    impl LlmEngine for SecondNoop {
        fn id(&self) -> &str { "noop2" }
        fn capabilities(&self) -> EngineCaps { EngineCaps { models: vec![] } }
        async fn complete(&self, _r: LlmRequest) -> Result<LlmResponse, EngineError> {
            Ok(LlmResponse { text: String::new() })
        }
    }

    pub struct SecondNoopStt;
    #[async_trait]
    impl SttEngine for SecondNoopStt {
        fn id(&self) -> &str {
            "noop-stt2"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn transcribe(
            &self,
            audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            Ok(Transcript {
                text: format!("heard: {} samples", audio.samples.len()),
            })
        }
    }

    pub struct SecondNoopTts;
    #[async_trait]
    impl TtsEngine for SecondNoopTts {
        fn id(&self) -> &str {
            "noop-tts2"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            Ok(AudioPcm {
                samples: vec![0.0; text.len() * 100],
                sample_rate_hz: 24_000,
            })
        }
    }
}
