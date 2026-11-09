//! Styles: the one concept that says how KEA rewrites selected text.
//!
//! A style is either a built-in [`RewriteMode`] (Improve, Fix grammar, …) or a
//! *custom style* the user wrote: a name and an instruction. Everything that
//! has to name "the style to use" — the saved active style, an app profile's
//! override, the `style` parameter of the local API — stores one string that
//! is a [`StyleRef`], so there is exactly one place that decides whether it
//! means a built-in or a custom one.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;
use crate::rewrite::mode::RewriteMode;
use crate::store::settings::SettingsRepo;

/// The setting holding the active style: a [`StyleRef`] string, so it names
/// either a built-in mode (`"improve"`) or a custom style id.
///
/// The key predates custom styles and keeps its name on purpose: every
/// install already has a value there, and the value space only widened.
pub const ACTIVE_STYLE_KEY: &str = "rewrite.active_mode";

/// A style the user wrote: a name for the picker and an instruction that is
/// used as the rewrite prompt, followed by the selected text.
///
/// A custom style has no AI of its own — it runs on the Rewrite AI like every
/// built-in style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CustomStyle {
    pub id: String,
    pub name: String,
    pub instruction: String,
}

impl CustomStyle {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            instruction: instruction.into(),
        }
    }
}

/// A stored or requested style: a built-in mode, or the id of a custom style.
///
/// Parsing never touches the database, so a `Custom` may name a style that
/// does not exist (deleted, or written by a different install). Whoever acts
/// on it checks with [`StyleRepo::exists`] and decides what that means for
/// their caller: the saved selection falls back to Improve, an explicit API
/// request is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleRef {
    Builtin(RewriteMode),
    Custom(String),
}

impl StyleRef {
    /// A built-in mode's name wins over a custom id of the same spelling, and
    /// [`StyleRepo::upsert`] refuses such ids so the case cannot arise.
    pub fn parse(value: &str) -> Self {
        match RewriteMode::from_str(value) {
            Some(mode) => StyleRef::Builtin(mode),
            None => StyleRef::Custom(value.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            StyleRef::Builtin(mode) => mode.as_str(),
            StyleRef::Custom(id) => id,
        }
    }
}

impl From<RewriteMode> for StyleRef {
    fn from(mode: RewriteMode) -> Self {
        StyleRef::Builtin(mode)
    }
}

pub struct StyleRepo {
    pool: SqlitePool,
    settings: SettingsRepo,
}

impl StyleRepo {
    pub fn new(pool: SqlitePool) -> Self {
        let settings = SettingsRepo::new(pool.clone());
        Self { pool, settings }
    }

    /// Creates or replaces a custom style, after the checks the UI also runs
    /// so a caller that skips the form cannot store what the form refuses.
    pub async fn upsert(&self, style: &CustomStyle) -> Result<(), KeaError> {
        let id = style.id.trim();
        let name = style.name.trim();
        if id.is_empty() {
            return Err(KeaError::Other("a style needs an id".into()));
        }
        if RewriteMode::from_str(id).is_some() {
            return Err(KeaError::Other(format!(
                "'{id}' is the name of a built-in style and cannot be a custom style id"
            )));
        }
        if name.is_empty() {
            return Err(KeaError::Other("a style needs a name".into()));
        }
        if style.instruction.trim().is_empty() {
            return Err(KeaError::Other("a style needs an instruction".into()));
        }
        let taken: Option<String> = sqlx::query_scalar(
            "SELECT id FROM rewrite_styles WHERE lower(name) = lower(?) AND id <> ?",
        )
        .bind(name)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if taken.is_some() {
            return Err(KeaError::Other(format!(
                "a style named '{name}' already exists"
            )));
        }
        sqlx::query(
            "INSERT INTO rewrite_styles(id, name, instruction) VALUES(?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               instruction = excluded.instruction",
        )
        .bind(id)
        .bind(name)
        .bind(&style.instruction)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<CustomStyle>, KeaError> {
        Ok(sqlx::query_as::<_, CustomStyle>(
            "SELECT id, name, instruction FROM rewrite_styles \
             ORDER BY sort_order, name COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get(&self, id: &str) -> Result<Option<CustomStyle>, KeaError> {
        Ok(sqlx::query_as::<_, CustomStyle>(
            "SELECT id, name, instruction FROM rewrite_styles WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Whether `style` can be run: always for a built-in, and for a custom
    /// style only while its row exists.
    pub async fn exists(&self, style: &StyleRef) -> Result<bool, KeaError> {
        match style {
            StyleRef::Builtin(_) => Ok(true),
            StyleRef::Custom(id) => Ok(self.get(id).await?.is_some()),
        }
    }

    /// Deletes a custom style and everything that selected it.
    ///
    /// The active style falls back to Improve and an app profile that forced
    /// the style goes back to inheriting, in the same transaction as the
    /// delete — so nothing is left naming an id that `build_llm_request`
    /// would later fail on.
    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM rewrite_styles WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE app_profiles SET rewrite_style = NULL WHERE rewrite_style = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let active: Option<String> = sqlx::query_scalar(
            "SELECT CASE WHEN json_valid(value) THEN json_extract(value, '$') END \
             FROM settings WHERE key = ?",
        )
        .bind(ACTIVE_STYLE_KEY)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        if active.as_deref() == Some(id) {
            let improve = serde_json::to_string(RewriteMode::Improve.as_str())?;
            sqlx::query("UPDATE settings SET value = ? WHERE key = ?")
                .bind(improve)
                .bind(ACTIVE_STYLE_KEY)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The saved active style. An unset value, or a custom style that no
    /// longer exists, is Improve — never an error, because the rewrite
    /// hotkey must still do something sensible after a style is deleted.
    pub async fn active(&self) -> Result<StyleRef, KeaError> {
        let stored = self.settings.get_optional::<String>(ACTIVE_STYLE_KEY).await;
        let style = match stored {
            Ok(Some(value)) if !value.is_empty() => StyleRef::parse(&value),
            _ => return Ok(StyleRef::Builtin(RewriteMode::Improve)),
        };
        if self.exists(&style).await? {
            Ok(style)
        } else {
            Ok(StyleRef::Builtin(RewriteMode::Improve))
        }
    }

    pub async fn set_active(&self, style: &StyleRef) -> Result<(), KeaError> {
        if !self.exists(style).await? {
            return Err(KeaError::NotFound(format!("style {}", style.as_str())));
        }
        self.settings
            .set(ACTIVE_STYLE_KEY, &style.as_str().to_string())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_context::AppProfile;
    use crate::store::app_profiles::AppProfileRepo;
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> (StyleRepo, SqlitePool) {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        (StyleRepo::new(pool.clone()), pool)
    }

    #[test]
    fn a_style_string_names_a_builtin_or_a_custom_id() {
        assert_eq!(
            StyleRef::parse("fix_grammar"),
            StyleRef::Builtin(RewriteMode::FixGrammar)
        );
        assert_eq!(
            StyleRef::parse("style-123"),
            StyleRef::Custom("style-123".into())
        );
        for mode in RewriteMode::ALL {
            assert_eq!(StyleRef::parse(mode.as_str()).as_str(), mode.as_str());
        }
    }

    #[tokio::test]
    async fn custom_style_round_trips() {
        let (styles, _) = repo().await;
        styles
            .upsert(&CustomStyle::new("s1", "Formal", "Be formal"))
            .await
            .unwrap();
        assert_eq!(
            styles.list().await.unwrap(),
            vec![CustomStyle::new("s1", "Formal", "Be formal")]
        );
        styles
            .upsert(&CustomStyle::new("s1", "Formal+", "Be very formal"))
            .await
            .unwrap();
        let got = styles.get("s1").await.unwrap().unwrap();
        assert_eq!(got.name, "Formal+");
        assert_eq!(got.instruction, "Be very formal");
    }

    #[tokio::test]
    async fn a_style_needs_a_name_an_instruction_and_a_unique_name() {
        let (styles, _) = repo().await;
        styles
            .upsert(&CustomStyle::new("s1", "Formal", "Be formal"))
            .await
            .unwrap();
        assert!(styles
            .upsert(&CustomStyle::new("s2", "  ", "x"))
            .await
            .is_err());
        assert!(styles
            .upsert(&CustomStyle::new("s2", "Casual", " \n "))
            .await
            .is_err());
        // Case-insensitive, and another style's name — but a style may keep
        // its own name when it is edited.
        let err = styles
            .upsert(&CustomStyle::new("s2", "formal", "Be formal too"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
        styles
            .upsert(&CustomStyle::new("s1", "FORMAL", "Be formal"))
            .await
            .unwrap();
        // An id spelled like a built-in would shadow it in `StyleRef::parse`.
        assert!(styles
            .upsert(&CustomStyle::new("improve", "Mine", "x"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_active_style_defaults_to_improve_and_follows_set_active() {
        let (styles, _) = repo().await;
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Builtin(RewriteMode::Improve)
        );
        styles
            .set_active(&StyleRef::Builtin(RewriteMode::Concise))
            .await
            .unwrap();
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Builtin(RewriteMode::Concise)
        );
        styles
            .upsert(&CustomStyle::new("s1", "Formal", "Be formal"))
            .await
            .unwrap();
        styles
            .set_active(&StyleRef::Custom("s1".into()))
            .await
            .unwrap();
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Custom("s1".into())
        );
        assert!(styles
            .set_active(&StyleRef::Custom("nope".into()))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn deleting_the_active_style_falls_back_to_improve() {
        let (styles, pool) = repo().await;
        styles
            .upsert(&CustomStyle::new("s1", "Formal", "Be formal"))
            .await
            .unwrap();
        styles
            .set_active(&StyleRef::Custom("s1".into()))
            .await
            .unwrap();
        styles.delete("s1").await.unwrap();
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Builtin(RewriteMode::Improve)
        );
        // The stored value is repaired too, not just masked on read.
        let raw: String = SettingsRepo::new(pool)
            .get(ACTIVE_STYLE_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw, "improve");
    }

    #[tokio::test]
    async fn deleting_another_style_leaves_the_active_one() {
        let (styles, _) = repo().await;
        styles
            .upsert(&CustomStyle::new("s1", "A", "a"))
            .await
            .unwrap();
        styles
            .upsert(&CustomStyle::new("s2", "B", "b"))
            .await
            .unwrap();
        styles
            .set_active(&StyleRef::Custom("s1".into()))
            .await
            .unwrap();
        styles.delete("s2").await.unwrap();
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Custom("s1".into())
        );
    }

    #[tokio::test]
    async fn a_dangling_active_style_reads_as_improve() {
        // A value written by another build, or by hand: it must not break
        // the rewrite hotkey.
        let (styles, pool) = repo().await;
        SettingsRepo::new(pool)
            .set(ACTIVE_STYLE_KEY, &"gone".to_string())
            .await
            .unwrap();
        assert_eq!(
            styles.active().await.unwrap(),
            StyleRef::Builtin(RewriteMode::Improve)
        );
    }

    #[tokio::test]
    async fn deleting_a_style_clears_the_profiles_that_forced_it() {
        let (styles, pool) = repo().await;
        styles
            .upsert(&CustomStyle::new("s1", "Terse", "Be terse."))
            .await
            .unwrap();
        let profiles = AppProfileRepo::new(pool);
        let mut p = AppProfile::new("slack", "Slack");
        p.rewrite_style = Some("s1".into());
        profiles.upsert(&p).await.unwrap();
        let mut other = AppProfile::new("mail", "Mail");
        other.rewrite_style = Some("professional".into());
        profiles.upsert(&other).await.unwrap();

        styles.delete("s1").await.unwrap();
        assert_eq!(
            profiles.get("slack").await.unwrap().unwrap().rewrite_style,
            None
        );
        assert_eq!(
            profiles.get("mail").await.unwrap().unwrap().rewrite_style,
            Some("professional".into())
        );
    }
}
