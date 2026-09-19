use crate::feature::{Command, Feature};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct FeatureRegistry {
    features: HashMap<String, Arc<dyn Feature>>,
}

impl FeatureRegistry {
    pub fn register(&mut self, f: Arc<dyn Feature>) {
        self.features.insert(f.id().to_string(), f);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn Feature>> {
        self.features.get(id).cloned()
    }
    /// The command a `(feature, command)` pair names, when the feature is
    /// registered and declares it.
    ///
    /// The features own their commands — id, title and compiled-in default
    /// accelerator — so the app shell looks them up here instead of keeping a
    /// second copy of the same table.
    pub fn find_command(&self, feature: &str, command: &str) -> Option<Command> {
        self.features
            .get(feature)?
            .commands()
            .into_iter()
            .find(|c| c.id == command)
    }
    pub fn list_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.features.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{CapKind, CapSlot, Feature};
    use std::sync::Arc;

    use crate::feature::Command;

    struct Fake;
    impl Feature for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn required_caps(&self) -> Vec<CapSlot> {
            vec![CapSlot {
                name: "llm",
                kind: CapKind::Llm,
            }]
        }
        fn commands(&self) -> Vec<Command> {
            vec![Command {
                id: "do_it".into(),
                title: "Do It".into(),
                default_accelerator: Some("Alt+K".into()),
            }]
        }
    }

    #[test]
    fn register_and_list() {
        let mut reg = FeatureRegistry::default();
        reg.register(Arc::new(Fake));
        assert_eq!(reg.list_ids(), vec!["fake".to_string()]);
        assert_eq!(reg.get("fake").unwrap().required_caps().len(), 1);
    }

    #[test]
    fn find_command_returns_the_declaring_features_command() {
        let mut reg = FeatureRegistry::default();
        reg.register(Arc::new(Fake));
        let cmd = reg.find_command("fake", "do_it").expect("command");
        assert_eq!(cmd.default_accelerator.as_deref(), Some("Alt+K"));
        assert_eq!(reg.find_command("fake", "nope"), None);
        assert_eq!(reg.find_command("nope", "do_it"), None);
    }
}
