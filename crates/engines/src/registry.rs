use crate::llm::StreamingLlmEngine;
use crate::traits::{LlmEngine, StreamingSttEngine, SttEngine, TtsEngine};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct EngineRegistry {
    llm: HashMap<String, Arc<dyn LlmEngine>>,
    stt: HashMap<String, Arc<dyn SttEngine>>,
    /// Kept apart from `stt` rather than folded into it: nothing *binds* to a
    /// streaming engine and no slot resolves to one, so a streaming engine in
    /// the STT map would be offered as a dictation engine the moment a picker
    /// listed `list_stt_ids`.
    streaming: HashMap<String, Arc<dyn StreamingSttEngine>>,
    tts: HashMap<String, Arc<dyn TtsEngine>>,
    /// The streaming half of an LLM engine, keyed by the *same* id as its
    /// buffered half.
    ///
    /// A second map rather than a second trait object in `llm`, for the
    /// reason `streaming` above is separate from `stt`: not every LLM backend
    /// can stream, so a caller that wants live text looks here and falls back
    /// to `llm(id)` when there is nothing — a visible downgrade rather than an
    /// engine that answers "unsupported" at the moment of use.
    streaming_llm: HashMap<String, Arc<dyn StreamingLlmEngine>>,
}

impl EngineRegistry {
    pub fn register_llm(&mut self, e: Arc<dyn LlmEngine>) {
        self.llm.insert(e.id().to_string(), e);
    }
    pub fn llm(&self, id: &str) -> Option<Arc<dyn LlmEngine>> {
        self.llm.get(id).cloned()
    }
    pub fn list_llm_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.llm.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn register_streaming_llm(&mut self, e: Arc<dyn StreamingLlmEngine>) {
        self.streaming_llm.insert(e.id().to_string(), e);
    }

    /// The streaming half of the engine with this id, if it has one.
    pub fn streaming_llm(&self, id: &str) -> Option<Arc<dyn StreamingLlmEngine>> {
        self.streaming_llm.get(id).cloned()
    }

    pub fn list_streaming_llm_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.streaming_llm.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn register_stt(&mut self, e: Arc<dyn SttEngine>) {
        self.stt.insert(e.id().to_string(), e);
    }
    pub fn stt(&self, id: &str) -> Option<Arc<dyn SttEngine>> {
        self.stt.get(id).cloned()
    }
    pub fn list_stt_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.stt.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn register_streaming_stt(&mut self, e: Arc<dyn StreamingSttEngine>) {
        self.streaming.insert(e.id().to_string(), e);
    }

    pub fn streaming_stt(&self, id: &str) -> Option<Arc<dyn StreamingSttEngine>> {
        self.streaming.get(id).cloned()
    }

    /// The one streaming engine, whichever it is.
    ///
    /// The caller has no engine id to offer: a streaming model is chosen by a
    /// setting, not by a binding, so there is nothing that names an engine.
    /// Returns `None` in a build with none registered, which is the case the
    /// whole feature is written around.
    pub fn any_streaming_stt(&self) -> Option<Arc<dyn StreamingSttEngine>> {
        let mut ids: Vec<&String> = self.streaming.keys().collect();
        ids.sort();
        ids.first().and_then(|id| self.streaming.get(*id)).cloned()
    }

    pub fn list_streaming_stt_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.streaming.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn register_tts(&mut self, e: Arc<dyn TtsEngine>) {
        self.tts.insert(e.id().to_string(), e);
    }

    pub fn tts(&self, id: &str) -> Option<Arc<dyn TtsEngine>> {
        self.tts.get(id).cloned()
    }

    pub fn list_tts_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.tts.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoopLlmEngine;
    use std::sync::Arc;

    #[test]
    fn register_and_lookup_llm() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        assert_eq!(reg.list_llm_ids(), vec!["noop".to_string()]);
        assert!(reg.llm("noop").is_some());
        assert!(reg.llm("missing").is_none());
    }
}

#[cfg(test)]
mod stt_registry_tests {
    use super::*;
    use crate::noop::NoopSttEngine;
    use crate::traits::{AudioPcm, SttOpts};

    #[tokio::test]
    async fn register_and_transcribe_noop_stt() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(NoopSttEngine));
        assert_eq!(reg.list_stt_ids(), vec!["noop-stt".to_string()]);
        let engine = reg.stt("noop-stt").unwrap();
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.1; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts::default(),
            )
            .await
            .unwrap();
        assert!(out.text.contains("100"));
    }
}

#[cfg(test)]
mod tts_registry_tests {
    use super::*;
    use crate::noop::NoopTtsEngine;
    use crate::traits::TtsOpts;

    #[tokio::test]
    async fn register_and_synthesize_noop_tts() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));
        assert_eq!(reg.list_tts_ids(), vec!["noop-tts".to_string()]);
        let pcm = reg
            .tts("noop-tts")
            .unwrap()
            .synthesize("hi", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 24_000);
    }
}
