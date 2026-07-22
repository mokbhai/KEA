#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CapKind { Llm, Stt, Tts }

#[derive(Debug, Clone, serde::Serialize)]
pub struct CapSlot { pub name: &'static str, pub kind: CapKind }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Command {
    pub id: String,
    pub title: String,
    pub default_accelerator: Option<String>,
}

pub trait Feature: Send + Sync {
    fn id(&self) -> &str;
    fn required_caps(&self) -> Vec<CapSlot>;
    fn commands(&self) -> Vec<Command> {
        vec![]
    }
}
