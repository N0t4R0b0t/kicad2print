//! Board scale factor resolution.
//!
//! The board is always output at 1:1 scale by default. Component pad spacing
//! is a physical constraint: scaling the board would move holes apart and make
//! real components impossible to insert.
//!
//! Channel width is independent of board scale — it is set by `channel_width_mm`
//! in the config and applied to every trace groove regardless of the original
//! KiCad trace width.
//!
//! `--scale` is available as a manual override for cases where the user
//! deliberately wants a scaled model (e.g. a scale mock-up), but it will
//! always warn that component placement is no longer valid.

use crate::config::Config;
use crate::pcb::PcbData;

/// Returns the effective scale factor to apply to the board geometry.
///
/// - Default (`scale_factor == 0`): returns 1.0 — board is printed at true size.
/// - Explicit `--scale N`: returns N and prints a warning that component
///   hole spacing will no longer match real components.
pub fn compute_scale_factor(_pcb: &PcbData, config: &Config) -> f64 {
    if config.scale_factor > 0.0 {
        if (config.scale_factor - 1.0).abs() > 0.001 {
            eprintln!(
                "⚠️  Scale factor {:.2}x applied — component hole spacing will NOT match real parts.",
                config.scale_factor
            );
        }
        config.scale_factor
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_1x() {
        let pcb = PcbData::default();
        let config = Config::default();
        assert_eq!(compute_scale_factor(&pcb, &config), 1.0);
    }

    #[test]
    fn test_explicit_scale_honored() {
        let pcb = PcbData::default();
        let mut config = Config::default();
        config.scale_factor = 2.5;
        assert!((compute_scale_factor(&pcb, &config) - 2.5).abs() < 1e-9);
    }
}
