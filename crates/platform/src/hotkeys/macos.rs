//! macOS global hotkeys via the `global-hotkey` crate (Carbon backend).
//!
//! # Manual verification
//! 1. Grant Accessibility permission to the host app in System Settings.
//! 2. Register a binding (e.g. `Cmd+Shift+R`) and call `on_action()` in a loop.
//! 3. Press the shortcut in any app; the corresponding [`super::ActionId`] should arrive on the channel.
//! 4. Headless CI cannot assert real key delivery — parser tests cover pure logic.

use super::{parse_accelerator, ActionId, HotkeyBinding, HotkeyError, Hotkeys};
use async_trait::async_trait;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc;

pub struct MacHotkeys {
    manager: Option<GlobalHotKeyManager>,
    by_accel: HashMap<String, HotKey>,
    by_id: Arc<Mutex<HashMap<u32, ActionId>>>,
    action_rx: Mutex<Option<mpsc::Receiver<ActionId>>>,
    _listener: Option<JoinHandle<()>>,
}

impl MacHotkeys {
    pub fn new() -> Self {
        let (action_tx, action_rx) = mpsc::channel(32);
        let by_id = Arc::new(Mutex::new(HashMap::<u32, ActionId>::new()));

        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(error) => {
                tracing::warn!(%error, "GlobalHotKeyManager unavailable; hotkey registration will fail");
                None
            }
        };

        let listener = manager.as_ref().map(|_| spawn_listener(by_id.clone(), action_tx.clone()));

        Self {
            manager,
            by_accel: HashMap::new(),
            by_id,
            action_rx: Mutex::new(Some(action_rx)),
            _listener: listener,
        }
    }
}

fn spawn_listener(
    by_id: Arc<Mutex<HashMap<u32, ActionId>>>,
    action_tx: mpsc::Sender<ActionId>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            match receiver.recv() {
                Ok(event) if event.state == HotKeyState::Pressed => {
                    if let Some(action) = by_id.lock().unwrap_or_else(|p| p.into_inner()).get(&event.id()).cloned() {
                        if action_tx.blocking_send(action).is_err() {
                            break;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

#[async_trait]
impl Hotkeys for MacHotkeys {
    fn register(&mut self, binding: HotkeyBinding, action: ActionId) -> Result<(), HotkeyError> {
        let hotkey = parse_accelerator(&binding.accelerator)?;
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| HotkeyError::Other("hotkey manager unavailable".into()))?;

        if let Some(previous) = self.by_accel.get(&binding.accelerator) {
            manager
                .unregister(*previous)
                .map_err(|e| HotkeyError::Other(e.to_string()))?;
            self.by_id.lock().unwrap_or_else(|p| p.into_inner()).remove(&previous.id());
        }

        manager
            .register(hotkey)
            .map_err(|e| HotkeyError::Other(e.to_string()))?;

        self.by_id
            .lock()
            .unwrap()
            .insert(hotkey.id(), action);
        self.by_accel.insert(binding.accelerator, hotkey);
        Ok(())
    }

    fn unregister(&mut self, binding: &HotkeyBinding) -> Result<(), HotkeyError> {
        let hotkey = self
            .by_accel
            .remove(&binding.accelerator)
            .ok_or_else(|| HotkeyError::NotRegistered(binding.accelerator.clone()))?;

        if let Some(manager) = self.manager.as_ref() {
            manager
                .unregister(hotkey)
                .map_err(|e| HotkeyError::Other(e.to_string()))?;
        }

        self.by_id.lock().unwrap_or_else(|p| p.into_inner()).remove(&hotkey.id());
        Ok(())
    }

    fn on_action(&self) -> mpsc::Receiver<ActionId> {
        self.action_rx
            .lock()
            .unwrap()
            .take()
            .expect("on_action may only be called once")
    }
}
