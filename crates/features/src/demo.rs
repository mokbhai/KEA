use kea_engines::{EngineRegistry, LlmRequest};
use crate::feature::{CapKind, CapSlot, Feature};

pub struct DemoFeature;

impl Feature for DemoFeature {
    fn id(&self) -> &str { "demo" }
    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot { name: "llm", kind: CapKind::Llm }]
    }
}

pub async fn run_ping(engines: &EngineRegistry, engine_id: &str, prompt: &str)
    -> Result<String, String>
{
    let engine = engines.llm(engine_id)
        .ok_or_else(|| format!("no llm engine '{engine_id}'"))?;
    let resp = engine.complete(LlmRequest { prompt: prompt.to_string(), model: None })
        .await.map_err(|e| e.to_string())?;
    Ok(resp.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_engines::{EngineRegistry, noop::NoopLlmEngine};
    use crate::feature::Feature;
    use std::sync::Arc;

    #[test]
    fn declares_one_llm_slot() {
        let f = DemoFeature;
        assert_eq!(f.id(), "demo");
        assert_eq!(f.required_caps().len(), 1);
    }

    #[tokio::test]
    async fn run_ping_routes_through_resolved_engine() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        let out = run_ping(&reg, "noop", "hi").await.unwrap();
        assert_eq!(out, "echo: hi");
    }
}
