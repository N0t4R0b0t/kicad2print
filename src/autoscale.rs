//! Board scaling calculation.
//!
//! Automatically determines the minimum scale factor needed to ensure that
//! even the narrowest trace can accommodate a groove of the configured width.
//!
//! For example, if the board has 0.1mm traces and we want 1.2mm wide channels,
//! we need to scale the entire board up by 12x.

use crate::config::Config;
use crate::pcb::PcbData;

/// Computes the scale factor needed for the PCB design.
///
/// If the user explicitly specified a `scale_factor > 0` in the config,
/// that value is returned unchanged.
///
/// Otherwise, this function computes the minimum scale factor needed to ensure
/// that the narrowest trace in the design is at least `config.channel_width_mm` wide.
///
/// # Algorithm
/// 1. Find the minimum trace width across all copper traces (F.Cu and B.Cu)
/// 2. If min trace width < configured channel width, scale = channel_width / min_trace_width
/// 3. Otherwise, scale = 1.0 (no scaling needed)
///
/// # Example
/// ```no_run
/// // Board has 0.2mm traces, config wants 1.0mm channels
/// // Result: scale = 1.0 / 0.2 = 5.0x
/// let scale = compute_scale_factor(&pcb, &config);
/// println!("Board will be scaled {}x", scale);
/// ```
pub fn compute_scale_factor(pcb: &PcbData, config: &Config) -> f64 {
    // If user explicitly set a scale factor, use it
    if config.scale_factor > 0.0 {
        return config.scale_factor;
    }

    // Find the minimum trace width
    let mut min_trace_width = f64::INFINITY;

    for trace in &pcb.traces_fcu {
        min_trace_width = min_trace_width.min(trace.width);
    }

    for trace in &pcb.traces_bcu {
        min_trace_width = min_trace_width.min(trace.width);
    }

    // If no traces found or width is unrealistic, don't scale
    if min_trace_width == f64::INFINITY || min_trace_width <= 0.0 {
        return 1.0;
    }

    // Calculate scale: channel_width must be at least config.channel_width_mm
    // So if trace_width < channel_width_mm, we scale up
    let required_scale = config.channel_width_mm / min_trace_width;

    // Don't scale down, only up
    if required_scale > 1.0 {
        eprintln!(
            "⚠️  Auto-scaling board {:.2}x to fit traces into channels",
            required_scale
        );
        eprintln!(
            "   Minimum trace width: {:.3}mm → Channel width: {:.3}mm",
            min_trace_width, config.channel_width_mm
        );
        required_scale
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_scaling_needed() {
        let mut pcb = PcbData::default();
        let config = Config::default();

        // Add a trace that's wide enough
        pcb.traces_fcu.push(crate::pcb::Trace {
            layer: crate::pcb::CopperLayer::FCu,
            start: crate::pcb::Point2::new(0.0, 0.0),
            end: crate::pcb::Point2::new(10.0, 0.0),
            width: 2.0, // Wide trace
        });

        let scale = compute_scale_factor(&pcb, &config);
        assert_eq!(scale, 1.0);
    }

    #[test]
    fn test_scaling_needed() {
        let mut pcb = PcbData::default();
        let config = Config::default(); // channel_width = 1.2mm

        // Add a very narrow trace
        pcb.traces_fcu.push(crate::pcb::Trace {
            layer: crate::pcb::CopperLayer::FCu,
            start: crate::pcb::Point2::new(0.0, 0.0),
            end: crate::pcb::Point2::new(10.0, 0.0),
            width: 0.1, // Narrow trace
        });

        let scale = compute_scale_factor(&pcb, &config);
        assert!(scale > 1.0);
        assert!((scale - 12.0).abs() < 0.001); // 1.2 / 0.1 = 12.0
    }
}
