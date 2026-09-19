use kea_engines::LlmRequest;

use crate::error::KeaError;
use crate::rewrite::catalog::{PromptCatalog, PromptVars};
use crate::rewrite::mode::RewriteMode;
use crate::rewrite::overrides::PromptOverrideRepo;
use crate::rewrite::preset::PresetRepo;

#[derive(Debug, Clone)]
pub struct RewriteInput {
    pub source_text: String,
    pub mode: RewriteMode,
    pub preset_id: Option<String>,
    /// The mode's one parameter, whatever it means for that mode — Ask KEA's
    /// instruction, Translate's BCP-47 target tag, nothing at all for the rest.
    ///
    /// Which template variable it fills is decided by
    /// [`PromptVars::for_mode`] from [`RewriteMode::parameter`], never by the
    /// producer, so a new templated mode is a row in that descriptor rather
    /// than another field here and another conditional at every caller.
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
        let vars = PromptVars::for_mode(input.mode, input.custom_instruction.as_deref());
        PromptCatalog::rendered(
            input.mode,
            &input.source_text,
            &vars,
            override_prompt.as_deref(),
        )?
    };
    // model and provider_ref are the resolved binding's to fill in — the
    // prompt builder has no view of which engine or provider will run it.
    Ok(LlmRequest {
        prompt,
        model: None,
        provider_ref: None,
    })
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

    #[tokio::test]
    async fn translate_renders_the_target_from_the_parameter_slot() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let presets = PresetRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            preset_id: None,
            custom_instruction: Some("de".into()),
        };
        let req = build_llm_request(&input, &presets, &overrides)
            .await
            .unwrap();
        assert!(req.prompt.contains("German"));
        assert!(req.prompt.contains("hello"));
        assert!(!req.prompt.contains("{{target_language}}"));
    }

    #[tokio::test]
    async fn a_preset_still_wins_over_translate() {
        // Precedence is unchanged by the new mode: a chosen preset replaces the
        // template whatever the mode says, so the two cannot both apply.
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let presets = PresetRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        presets
            .upsert(&RewritePreset {
                id: "p1".into(),
                name: "Pirate".into(),
                instruction: "Rewrite as a pirate".into(),
            })
            .await
            .unwrap();

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            preset_id: Some("p1".into()),
            custom_instruction: Some("de".into()),
        };
        let req = build_llm_request(&input, &presets, &overrides)
            .await
            .unwrap();
        assert!(req.prompt.contains("Rewrite as a pirate"));
        assert!(!req.prompt.contains("German"));
    }

    #[tokio::test]
    async fn translate_without_a_target_is_an_error_not_a_bare_prompt() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let presets = PresetRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            preset_id: None,
            custom_instruction: None,
        };
        let err = build_llm_request(&input, &presets, &overrides)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing target language"));
    }
}
