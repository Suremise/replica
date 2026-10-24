//! The tray's icons, compiled into the exe, so the portable copy needs no extra files.

use replica_win::tray::{IconFile, ThemedIcon};

use super::state::IconState;

macro_rules! themed {
    ($state:literal) => {
        ThemedIcon {
            light_taskbar: IconFile(include_bytes!(concat!(
                "../../../../assets/tray/",
                $state,
                "-on-light.ico"
            ))),
            dark_taskbar: IconFile(include_bytes!(concat!(
                "../../../../assets/tray/",
                $state,
                "-on-dark.ico"
            ))),
        }
    };
}

pub const IDLE: ThemedIcon = themed!("idle");
pub const RUNNING: [ThemedIcon; 2] = [themed!("running-1"), themed!("running-2")];
pub const FAILED: ThemedIcon = themed!("failed");
pub const OVERDUE: ThemedIcon = themed!("overdue");

/// The icon for a state; `frame` alternates the running animation.
pub fn for_state(state: IconState, frame: usize) -> ThemedIcon {
    match state {
        IconState::Running => RUNNING[frame % 2],
        IconState::Failed => FAILED,
        IconState::Overdue => OVERDUE,
        IconState::Idle => IDLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_every_tray_size() {
        let all = [IDLE, RUNNING[0], RUNNING[1], FAILED, OVERDUE];
        for icon in all {
            for file in [icon.light_taskbar, icon.dark_taskbar] {
                let sizes: Vec<u32> = file.images().iter().map(|i| i.0).collect();
                assert_eq!(sizes, [16, 20, 24, 32]);
            }
        }
    }
}
