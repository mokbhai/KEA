//! `NSWorkspace`-backed application launching.

use super::{AppLaunchError, AppLauncher};

pub struct MacAppLauncher;

impl AppLauncher for MacAppLauncher {
    fn open(&self, name: &str) -> Result<(), AppLaunchError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppLaunchError::NotFound {
                name: String::new(),
            });
        }

        // SAFETY: `sharedWorkspace` returns a shared, autoreleased object that
        // outlives the call, and the class lookup returns `None` rather than a
        // dangling pointer when AppKit is not loaded. `launchApplication:`
        // takes a borrowed `NSString` and returns a `BOOL`.
        //
        // `launchApplication:` is deprecated in favour of the async
        // `openApplicationAtURL:configuration:completionHandler:`. It is used
        // anyway because it is synchronous and total: it resolves a display
        // name against the user's installed applications and answers yes or no.
        // The replacement needs a resolved bundle URL — putting the name
        // resolution back on us, which is the part macOS is better at — and
        // delivers its answer on a completion handler, which would make this
        // async for no gain the caller can use.
        unsafe {
            let Some(class) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
                return Err(AppLaunchError::Failed {
                    name: name.to_string(),
                    reason: "AppKit is not loaded".into(),
                });
            };
            let workspace: *mut objc2::runtime::AnyObject =
                objc2::msg_send![class, sharedWorkspace];
            if workspace.is_null() {
                return Err(AppLaunchError::Failed {
                    name: name.to_string(),
                    reason: "no shared workspace".into(),
                });
            }

            let ns_name = objc2_foundation::NSString::from_str(name);
            let ok: bool = objc2::msg_send![workspace, launchApplication: &*ns_name];

            if ok {
                Ok(())
            } else {
                // `launchApplication:` reports a single BOOL, so a name that
                // resolved to nothing and a bundle that refused to start are
                // indistinguishable here. Reported as not-found because that
                // is overwhelmingly the common case downstream of speech
                // recognition, and it is the one the user can act on.
                Err(AppLaunchError::NotFound {
                    name: name.to_string(),
                })
            }
        }
    }
}
