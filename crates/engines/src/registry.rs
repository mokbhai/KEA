use std::collections::HashMap;
use std::sync::Arc;
use crate::traits::{LlmEngine, SttEngine, TtsEngine};

#[derive(Default)]
pub struct EngineRegistry {
    llm: HashMap<String, Arc<dyn LlmEngine>>,
    stt: HashMap<String, Arc<dyn SttEngine>>,
    tts: HashMap<String, Arc<dyn TtsEngine>>,
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
