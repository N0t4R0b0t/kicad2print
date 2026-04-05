//! `kicad2print` - Convert KiCad PCB designs to 3D-printable models.
//!
//! This CLI tool takes a KiCad `.kicad_pcb` file and generates 3D models (STL/3MF)
//! suitable for printing on an FDM 3D printer as part of the "hybrid PCB" construction
//! method using 3D-printed substrates, copper wire traces, and copper eyelets.
//!
//! # Quick Start
//!
//! ```sh
//! # Using defaults:
//! kicad2print my_board.kicad_pcb
//!
//! # Using a config file:
//! kicad2print my_board.kicad_pcb --config my_settings.toml
//!
//! # Overriding specific settings:
//! kicad2print my_board.kicad_pcb --channel-width 0.8 --eyelet-style hole
//!
//! # Generate both STL and 3MF:
//! kicad2print my_board.kicad_pcb --format both
//! ```
//!
//! # Configuration
//!
//! Settings can be customized via:
//! 1. A TOML config file (e.g., `kicad2print.toml`)
//! 2. Command-line arguments (override config file)
//! 3. Built-in defaults
//!
//! # Output
//!
//! Generated files appear in the `--output-dir` (default: `./output/`):
//! - `boardname_combined.stl` - Complete model with all features
//! - `boardname_top.stl` - Top layer only (for reference)
//! - `boardname_bottom.stl` - Bottom layer only (for reference)
//!
//! The model can be imported into any 3D slicer (Prusaslicer, Bambu Studio, etc.)
//! and printed on a standard FDM 3D printer.

mod autoscale;
mod config;
mod parser;
mod pcb;

use anyhow::{Context, Result};
use clap::Parser;
use config::{CliOverrides, Config, EyeletStyle, OutputFormat};
use std::path::PathBuf;

/// Command-line arguments for `kicad2print`.
///
/// Using `clap` with derive macros makes the argument parsing declarative and easy to read.
/// Each field automatically becomes a CLI option with the name derived from the field name
/// (with underscores converted to hyphens).
#[derive(Parser, Debug)]
#[command(
    name = "kicad2print",
    version = "0.1.0",
    about = "Convert KiCad PCB designs to 3D-printable STL/3MF models",
    long_about = "Transform a KiCad .kicad_pcb file into a 3D-printable substrate \
                  for hybrid PCB construction using wire traces and copper eyelets."
)]
struct Args {
    /// Path to the KiCad PCB file (.kicad_pcb)
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// Path to TOML config file (defaults to kicad2print.toml in current directory)
    ///
    /// If the file doesn't exist, built-in defaults are used.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Width of trace channels (millimeters)
    #[arg(long, value_name = "MM")]
    channel_width: Option<f64>,

    /// Depth of trace channels (millimeters)
    #[arg(long, value_name = "MM")]
    channel_depth: Option<f64>,

    /// Style of via/eyelet representation: 'hole' or 'indent'
    #[arg(long, value_name = "STYLE")]
    eyelet_style: Option<String>,

    /// Diameter of via holes or indent dimples (millimeters)
    #[arg(long, value_name = "MM")]
    eyelet_diameter: Option<f64>,

    /// Depth of shallow eyelet indents (millimeters, used when eyelet-style=indent)
    #[arg(long, value_name = "MM")]
    indent_depth: Option<f64>,

    /// Diameter of component pad through-holes (millimeters)
    #[arg(long, value_name = "MM")]
    pad_hole_diameter: Option<f64>,

    /// Total thickness of the printed substrate (millimeters)
    #[arg(long, value_name = "MM")]
    substrate_thickness: Option<f64>,

    /// Manual board scale factor (0 = auto-calculate)
    #[arg(long, value_name = "FACTOR")]
    scale: Option<f64>,

    /// Output format(s): 'stl', '3mf', or 'both'
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Directory for output files
    #[arg(long, value_name = "DIR")]
    output_dir: Option<String>,

    /// Verbose output (print detailed information during processing)
    #[arg(short, long)]
    verbose: bool,
}

impl Args {
    /// Converts command-line arguments into a `CliOverrides` struct for merging with config.
    ///
    /// Each `Some` value from CLI arguments becomes an override; `None` values
    /// are ignored (allowing config file values to take precedence).
    fn to_overrides(&self) -> Result<CliOverrides> {
        let eyelet_style = self
            .eyelet_style
            .as_ref()
            .map(|s| s.parse::<EyeletStyle>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid eyelet-style: {}", e))?;

        let output_format = self
            .format
            .as_ref()
            .map(|s| s.parse::<OutputFormat>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid output format: {}", e))?;

        Ok(CliOverrides {
            channel_width_mm: self.channel_width,
            channel_depth_mm: self.channel_depth,
            eyelet_style,
            eyelet_diameter_mm: self.eyelet_diameter,
            indent_depth_mm: self.indent_depth,
            pad_hole_diameter_mm: self.pad_hole_diameter,
            substrate_thickness_mm: self.substrate_thickness,
            scale_factor: self.scale,
            output_format,
            output_dir: self.output_dir.clone(),
        })
    }
}

/// Main entry point.
///
/// This function coordinates the entire pipeline:
/// 1. Parse command-line arguments
/// 2. Load and merge configuration
/// 3. Parse the KiCad PCB file
/// 4. Calculate scaling factor
/// 5. Generate 3D geometry
/// 6. Export to STL/3MF files
fn main() -> Result<()> {
    // Parse command-line arguments
    let args = Args::parse();

    // Step 1: Load configuration
    if args.verbose {
        println!("📋 Loading configuration...");
    }

    let config_path = args
        .config
        .as_ref()
        .map(|p| p.as_path())
        .unwrap_or_else(|| std::path::Path::new("kicad2print.toml"));

    let mut config = Config::from_file(config_path)?;

    // Step 2: Apply command-line overrides
    let overrides = args.to_overrides()?;
    config.merge_cli_overrides(&overrides);

    if args.verbose {
        config.print_summary();
    }

    // Step 3: Parse the KiCad PCB file
    if args.verbose {
        println!("\n📖 Parsing KiCad file: {}", args.input.display());
    }

    let pcb_data = parser::parse_pcb(&args.input)
        .context("Failed to parse KiCad file")?;

    if args.verbose {
        pcb_data.print_summary();
    }

    // Step 4: Validate that we have required data
    if pcb_data.outline.is_none() {
        eprintln!("⚠️  Warning: No board outline found (Edge.Cuts layer)");
        eprintln!("   The 3D model will use a bounding box of all traces.");
    }

    // Step 5: Calculate scale factor
    if args.verbose {
        println!("\n📏 Calculating scale factor...");
    }

    let scale_factor = autoscale::compute_scale_factor(&pcb_data, &config);
    if args.verbose {
        println!("   Scale factor: {:.2}x", scale_factor);
    }

    // Apply scaling to PCB data if needed
    let _pcb_data = if (scale_factor - 1.0).abs() > 0.001 {
        if args.verbose {
            println!("   Applying scaling...");
        }
        pcb_data.scale(scale_factor)
    } else {
        pcb_data
    };

    // Step 6: Generate 3D geometry
    if args.verbose {
        println!("\n🔧 Generating 3D geometry...");
    }

    // TODO: Implement geometry generation module
    println!("⚠️  Geometry generation not yet implemented!");
    println!("   (Next steps: create geometry/ module with substrate, channels, eyelets, pads)");

    // Step 7: Export to files
    if args.verbose {
        println!("\n💾 Exporting to {} format...", config.output_format);
    }

    // TODO: Implement export module
    println!("⚠️  Export not yet implemented!");
    println!("   (Next steps: create export/ module with STL and 3MF writers)");

    // Success message
    println!("\n✅ Processing complete!");
    println!("   Output files will be in: {}", config.output_dir);

    Ok(())
}
