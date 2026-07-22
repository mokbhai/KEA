use std::collections::HashMap;
use std::sync::Arc;
use crate::feature::Feature;

#[derive(Default)]
pub struct FeatureRegistry { features: HashMap<String, Arc<dyn Feature>> }

impl FeatureRegistry {
    pub fn register(&mut self, f: Arc<dyn Feature>) {
        self.features.insert(f.id().to_string(), f);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn Feature>> { self.features.get(id).cloned() }
    pub fn list_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.features.keys().cloned().collect();
        v.sort(); v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{CapKind, CapSlot, Feature};
    use std::sync::Arc;

    struct Fake;
    impl Feature for Fake {
        fn id(&self) -> &str { "fake" }
        fn required_caps(&self) -> Vec<CapSlot> {
            vec![CapSlot { name: "llm", kind: CapKind::Llm }]
        }
    }

    #[test]
    fn register_and_list() {
        let mut reg = FeatureRegistry::default();
        reg.register(Arc::new(Fake));
        assert_eq!(reg.list_ids(), vec!["fake".to_string()]);
        assert_eq!(reg.get("fake").unwrap().required_caps().len(), 1);
    }
}
