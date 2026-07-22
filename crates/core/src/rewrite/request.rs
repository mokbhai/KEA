use kea_engines::LlmRequest;

use crate::error::KeaError;
use crate::rewrite::catalog::PromptCatalog;
use crate::rewrite::mode::RewriteMode;
use crate::rewrite::overrides::PromptOverrideRepo;
use crate::rewrite::preset::PresetRepo;

#[derive(Debug, Clone)]
pub struct RewriteInput {
    pub source_text: String,
    pub mode: RewriteMode,
    pub preset_id: Option<String>,
    pub custom_instruction: Option<String>,
}

pub async fn build_llm_request(
    input: &RewriteInput,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
) -> Result<LlmRequest, KeaError> {
    let prompt = if let Some(ref preset_id) = input.preset_id {
        let preset = presets
            .get(preset_id)
            .await?
            .ok_or_else(|| KeaError::NotFound(format!("preset {preset_id}")))?;
        format!(
            "{}\n\nSource text:\n{}",
            preset.instruction, input.source_text
        )
    } else {
        let override_prompt = overrides.get(input.mode).await?;
        let custom = input.custom_instruction.as_deref();
        PromptCatalog::rendered(
            input.mode,
            &input.source_text,
            custom,
            override_prompt.as_deref(),
        )?
    };
    Ok(LlmRequest { prompt, model: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::preset::RewritePreset;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn preset_instruction_replaces_mode_template() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let presets = PresetRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        presets
            .upsert(&RewritePreset {
                id: "p1".into(),
                name: "French".into(),
                instruction: "Translate to French".into(),
            })
            .await
            .unwrap();

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            preset_id: Some("p1".into()),
            custom_instruction: None,
        };
        let req = build_llm_request(&input, &presets, &overrides)
            .await
            .unwrap();
        assert!(req.prompt.contains("Translate to French"));
        assert!(req.prompt.contains("hello"));
    }

    #[tokio::test]
    async fn mode_template_when_no_preset() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let presets = PresetRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            preset_id: None,
            custom_instruction: None,
        };
        let req = build_llm_request(&input, &presets, &overrides)
            .await
            .unwrap();
        assert!(req.prompt.contains("writing assistant"));
        assert!(req.prompt.contains("hello"));
    }
}
