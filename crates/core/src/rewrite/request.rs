use kea_engines::LlmRequest;

use crate::error::KeaError;
use crate::rewrite::catalog::{PromptCatalog, PromptVars};
use crate::rewrite::mode::RewriteMode;
use crate::rewrite::overrides::PromptOverrideRepo;
use crate::rewrite::style::{StyleRef, StyleRepo};

#[derive(Debug, Clone)]
pub struct RewriteInput {
    pub source_text: String,
    /// The built-in style to run. Ignored when `custom_style_id` is set.
    pub mode: RewriteMode,
    /// A custom style's id. When set, that style's instruction is the prompt
    /// and `mode` plays no part — a custom style replaces the built-in
    /// template outright, so the two cannot both apply.
    pub custom_style_id: Option<String>,
    /// The mode's one parameter, whatever it means for that mode — Ask KEA's
    /// instruction, Translate's BCP-47 target tag, nothing at all for the rest.
    ///
    /// Which template variable it fills is decided by
    /// [`PromptVars::for_mode`] from [`RewriteMode::parameter`], never by the
    /// producer, so a new templated mode is a row in that descriptor rather
    /// than another field here and another conditional at every caller.
    pub custom_instruction: Option<String>,
}

impl RewriteInput {
    /// An input running `style`: a built-in mode, or a custom style whose
    /// instruction replaces the template.
    ///
    /// `custom_instruction` is the built-in mode's parameter (Ask KEA's
    /// instruction, Translate's target) and means nothing to a custom style,
    /// which carries its whole prompt, so it is dropped for one. `mode` is a
    /// placeholder then: `custom_style_id` wins in [`build_llm_request`].
    pub fn for_style(
        style: StyleRef,
        source_text: String,
        custom_instruction: Option<String>,
    ) -> Self {
        match style {
            StyleRef::Builtin(mode) => Self {
                source_text,
                mode,
                custom_style_id: None,
                custom_instruction,
            },
            StyleRef::Custom(id) => Self {
                source_text,
                mode: RewriteMode::Improve,
                custom_style_id: Some(id),
                custom_instruction: None,
            },
        }
    }

    /// The style this input runs, the inverse of [`RewriteInput::for_style`].
    pub fn style(&self) -> StyleRef {
        match &self.custom_style_id {
            Some(id) => StyleRef::Custom(id.clone()),
            None => StyleRef::Builtin(self.mode),
        }
    }
}

pub async fn build_llm_request(
    input: &RewriteInput,
    styles: &StyleRepo,
    overrides: &PromptOverrideRepo,
) -> Result<LlmRequest, KeaError> {
    let (system, user) = if let Some(ref style_id) = input.custom_style_id {
        let style = styles
            .get(style_id)
            .await?
            .ok_or_else(|| KeaError::NotFound(format!("style {style_id}")))?;
        (style.instruction, input.source_text.clone())
    } else {
        let override_prompt = overrides.get(input.mode).await?;
        let vars = PromptVars::for_mode(input.mode, input.custom_instruction.as_deref());
        PromptCatalog::rendered_parts(
            input.mode,
            &input.source_text,
            &vars,
            override_prompt.as_deref(),
        )?
    };
    // model and provider_ref are the resolved binding's to fill in — the
    // request builder has no view of which engine or provider will run it.
    Ok(LlmRequest::single_turn(system, user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::style::CustomStyle;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn custom_style_instruction_replaces_mode_template() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        styles
            .upsert(&CustomStyle::new("p1", "French", "Translate to French"))
            .await
            .unwrap();

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            custom_style_id: Some("p1".into()),
            custom_instruction: None,
        };
        let req = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap();
        assert!(req.prompt().contains("Translate to French"));
        assert!(req.prompt().contains("hello"));
    }

    #[tokio::test]
    async fn a_custom_style_is_the_prompt_followed_by_the_source_text() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        styles
            .upsert(&CustomStyle::new(
                "style-1",
                "Standup",
                "Rewrite as a three-bullet standup update.",
            ))
            .await
            .unwrap();

        let input = RewriteInput {
            source_text: "did the thing".into(),
            mode: RewriteMode::Improve,
            custom_style_id: Some("style-1".into()),
            custom_instruction: None,
        };
        let req = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap();
        assert_eq!(
            req.prompt(),
            "Rewrite as a three-bullet standup update.\n\ndid the thing"
        );
    }

    #[tokio::test]
    async fn a_deleted_custom_style_is_an_error_naming_it() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            custom_style_id: Some("gone".into()),
            custom_instruction: None,
        };
        let err = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("style gone"));
    }

    #[tokio::test]
    async fn mode_template_when_no_custom_style() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            custom_style_id: None,
            custom_instruction: None,
        };
        let req = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap();
        assert!(req.prompt().contains("writing assistant"));
        assert!(req.prompt().contains("hello"));
    }

    #[tokio::test]
    async fn translate_renders_the_target_from_the_parameter_slot() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            custom_style_id: None,
            custom_instruction: Some("de".into()),
        };
        let req = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap();
        assert!(req.prompt().contains("German"));
        assert!(req.prompt().contains("hello"));
        assert!(!req.prompt().contains("{{target_language}}"));
    }

    #[tokio::test]
    async fn a_custom_style_still_wins_over_translate() {
        // Precedence is unchanged by the new mode: a chosen custom style replaces the
        // template whatever the mode says, so the two cannot both apply.
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);
        styles
            .upsert(&CustomStyle::new("p1", "Pirate", "Rewrite as a pirate"))
            .await
            .unwrap();

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            custom_style_id: Some("p1".into()),
            custom_instruction: Some("de".into()),
        };
        let req = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap();
        assert!(req.prompt().contains("Rewrite as a pirate"));
        assert!(!req.prompt().contains("German"));
    }

    #[tokio::test]
    async fn translate_without_a_target_is_an_error_not_a_bare_prompt() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let styles = StyleRepo::new(pool.clone());
        let overrides = PromptOverrideRepo::new(pool);

        let input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Translate,
            custom_style_id: None,
            custom_instruction: None,
        };
        let err = build_llm_request(&input, &styles, &overrides)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing target language"));
    }
}
