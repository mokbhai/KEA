//! The voice assistant: what it may do, and how a spoken request reaches it.

pub mod action;
pub mod args;
pub mod dispatch;
pub mod registry;
pub mod routing;
pub mod session;
pub mod settings;

pub use action::{duplicate_id, ActionSpec, ArgSpec, ArgType};
pub use args::{validate_args, ArgError, ArgValue, ResolvedArgs};
pub use dispatch::{
    plan, ActionFailure, ActionHandler, ActionOutcome, Disclosure, Dispatcher, RequestPlan,
};
pub use registry::{ActionRegistry, CATALOG};
pub use routing::{
    build_routing_prompt, parse_routing_response, RoutingOutcome, ANSWER_WORD_LIMIT,
};
pub use session::{Session, SessionEvent, SessionState};
pub use settings::{AssistantSettings, AssistantSettingsRepo};
