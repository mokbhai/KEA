pub mod settings;
pub mod vocabulary;

pub use settings::{DictationSettings, DictationSettingsRepo};
pub use vocabulary::{apply_vocabulary, hint_terms};
