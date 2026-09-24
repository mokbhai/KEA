//! The voice assistant: what it may do, and how a spoken request reaches it.

pub mod action;
pub mod args;
pub mod registry;
pub mod dispatch;
pub mod routing;
pub mod session;

pub use action::{duplicate_id, ActionSpec, ArgSpec, ArgType};
pub use args::{validate_args, ArgError, ArgValue, ResolvedArgs};
pub use registry::{ActionRegistry, CATALOG};
pub use dispatch::{
    plan, ActionFailure, ActionHandler, ActionOutcome, Disclosure, Dispatcher, RequestPlan,
};
pub use session::{Session, SessionEvent, SessionState};
pub use routing::{build_routing_prompt, parse_routing_response, RoutingOutcome, ANSWER_WORD_LIMIT};
