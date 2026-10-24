//! The window helpers against the real monitors.
#![cfg(windows)]

use replica_win::window::is_on_screen;

#[test]
fn the_primary_monitor_s_corner_is_on_screen_and_far_away_is_not() {
    assert!(is_on_screen([0, 0, 100, 100]));
    assert!(!is_on_screen([-200_000, -200_000, -199_900, -199_900]));
}
