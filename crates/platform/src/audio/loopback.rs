//! Virtual loopback input device detection (BlackHole, Loopback, monitor sources).

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Host};

/// Returns true when `name` matches a known system-audio loopback / monitor device.
pub fn is_loopback_device_name(name: &str) -> bool {
    name.contains("BlackHole")
        || name.contains("Loopback")
        || name.contains("Monitor")
}

/// Human-readable device name from cpal, when available.
pub fn device_display_name(device: &Device) -> Option<String> {
    device.name().ok()
}

/// Scan input devices for a loopback/monitor source suitable for system audio capture.
pub fn find_loopback_input_device(host: &Host) -> Option<Device> {
    let mut devices = host.input_devices().ok()?;
    devices.find(|device| {
        device_display_name(device)
            .map(|name| is_loopback_device_name(&name))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_blackhole_as_loopback() {
        assert!(is_loopback_device_name("BlackHole 2ch"));
        assert!(!is_loopback_device_name("MacBook Pro Microphone"));
    }

    #[test]
    fn recognizes_loopback_and_monitor_names() {
        assert!(is_loopback_device_name("Loopback Audio"));
        assert!(is_loopback_device_name("Monitor of Built-in Output"));
    }
}
