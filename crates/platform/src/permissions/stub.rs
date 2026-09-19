//! Non-macOS permission stub — all kinds report [`PermStatus::Unknown`].

use super::{PermError, PermKind, PermStatus, Permissions};
use async_trait::async_trait;

pub struct StubPermissions;

impl StubPermissions {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Permissions for StubPermissions {
    fn status(&self, _kind: PermKind) -> PermStatus {
        PermStatus::Unknown
    }

    async fn request(&self, _kind: PermKind) -> Result<PermStatus, PermError> {
        Ok(PermStatus::Unknown)
    }
}
