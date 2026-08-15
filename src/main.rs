// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

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
mod export;
mod geometry;
mod mcp;
mod parser;
mod pcb;
mod render;

use anyhow::{Context, Result};
use clap::Parser;
use config::{
    ChannelProfile, CliOverrides, Config, EyeletStyle, Mode, OutputFormat, StencilMount, ViaStyle,
};
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<()> {
    // Use tokio only when needed (MCP mode); for CLI skip the async runtime overhead
    if std::env::args().any(|a| a == "--mcp") {
        tokio::runtime::Runtime::new()?.block_on(async { mcp::run().await })
    } else {
        cli_main()
    }
}

/// Command-line arguments for `kicad2print`.
///
/// Using `clap` with derive macros makes the argument parsing declarative and easy to read.
/// Each field automatically becomes a CLI option with the name derived from the field name
/// (with underscores converted to hyphens).
#[derive(Parser, Debug)]
#[command(
    name = "kicad2print",
    // Sourced from Cargo.toml so it can never drift. It had been hardcoded at
    // "0.1.0" since the beginning, so every release reported the wrong version —
    // and the release workflow sets the version by rewriting Cargo.toml from the
    // git tag, which a hardcoded string here silently discarded.
    version = env!("CARGO_PKG_VERSION"),
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

    /// Construction mode: 'copper-wire' or 'electrolysis'
    ///
    /// Applies the matching preset from presets/ as the baseline — the file is
    /// compiled in, so --mode electrolysis is exactly equivalent to
    /// --config presets/electrolysis.toml.
    /// 'copper-wire' (default): wide channels for 30 AWG wire, wire-laying guide.
    /// 'electrolysis': narrow channels for electroplated copper, plating guide.
    /// Your own TOML overrides the preset, and CLI flags override both.
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,

    /// Width of trace channels (millimeters)
    #[arg(long, value_name = "MM")]
    channel_width: Option<f64>,

    /// Depth of trace channels (millimeters)
    #[arg(long, value_name = "MM")]
    channel_depth: Option<f64>,

    /// Trace groove cross-section: 'rect', 'trapezoid', or 'vee'.
    /// Sloped walls plate more evenly (a rectangular groove tends to leave the
    /// centre unfilled) and print better on a 0.4mm nozzle.
    #[arg(long, value_name = "PROFILE")]
    channel_profile: Option<String>,

    /// Groove floor width for --channel-profile=trapezoid (millimeters).
    /// The opening stays at --channel-width; this narrows only the bottom.
    #[arg(long, value_name = "MM")]
    channel_floor_width: Option<f64>,

    /// Step height for sloped walls (millimeters).
    /// Set this to your slicer's layer height: V grooves and cone countersinks
    /// are built as a stack of constant-width bands, and at layer height the
    /// printed result is identical to a smooth ramp. Band count is
    /// channel-depth / this, capped at 8, so very small values stop helping.
    #[arg(long, value_name = "MM")]
    taper_slice_height: Option<f64>,

    /// Countersink wall angle in degrees from the board surface (--via-style cone).
    /// 45 is the standard countersink angle and the steepest overhang an FDM
    /// printer holds without support.
    #[arg(long, value_name = "DEG")]
    cone_angle: Option<f64>,

    /// Straight section where the two cones meet (millimeters, --via-style cone).
    /// Stops them meeting at a fragile knife edge and gives the lead something
    /// to locate against.
    #[arg(long, value_name = "MM")]
    throat_height: Option<f64>,

    /// Material kept between a cone mouth and foreign-net copper or the board
    /// edge (millimeters). A correctness constraint, not cosmetic: merged
    /// mouths on different nets short together once plated.
    #[arg(long, value_name = "MM")]
    min_rim: Option<f64>,

    /// Through-hole barrel shape: 'straight' or 'cone'.
    /// 'cone' countersinks both faces so the barrel can be seed-painted and
    /// plated without an eyelet, and gives a solder cup on the underside.
    #[arg(long, value_name = "STYLE")]
    via_style: Option<String>,

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

    /// Generate only one side: 'top' or 'bottom' (omit for combined model)
    /// When set, output files are suffixed with _top or _bottom
    #[arg(long, value_name = "SIDE")]
    side: Option<String>,

    /// Directory for output files
    #[arg(long, value_name = "DIR")]
    output_dir: Option<String>,

    /// Verbose output (print detailed information during processing)
    #[arg(short, long)]
    verbose: bool,

    /// Open the generated model in a 3D viewer after conversion
    #[arg(long)]
    view: bool,

    /// Disable component pad through-holes (for rapid prototyping)
    #[arg(long)]
    no_pad_holes: bool,

    /// Disable shaped pad land indents (rect/circle/oval, for electroplating a
    /// proper solderable pad, not just the lead's round hole)
    #[arg(long)]
    no_pad_lands: bool,

    /// Disable via/eyelet indent guides
    #[arg(long)]
    no_via_indents: bool,

    /// Also generate a snap-on conductive-paint stencil (auto-enabled in
    /// electrolysis mode). Emits <stem>_stencil_top/bottom.stl (traces + holes).
    #[arg(long)]
    stencil: bool,

    /// Add the temporary plating bus (perimeter rail + stubs + tie-bars) to the
    /// stencil so every trace shorts to one cathode for electroplating. Off by
    /// default — the plain stencil masks only the traces and holes.
    #[arg(long)]
    plating_bus: bool,

    /// Stencil mount style: 'lip' (integral perimeter lip) or 'ring' (flat plates
    /// + a separate clamp ring; print contact-face-down for a smooth finish)
    #[arg(long, value_name = "STYLE")]
    stencil_mount: Option<String>,
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

        let via_style = self
            .via_style
            .as_ref()
            .map(|s| s.parse::<ViaStyle>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid via-style: {}", e))?;

        let channel_profile = self
            .channel_profile
            .as_ref()
            .map(|s| s.parse::<ChannelProfile>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid channel-profile: {}", e))?;

        let output_format = self
            .format
            .as_ref()
            .map(|s| s.parse::<OutputFormat>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid output format: {}", e))?;

        let mode = self
            .mode
            .as_ref()
            .map(|s| s.parse::<Mode>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid mode: {}", e))?;

        let stencil_mount = self
            .stencil_mount
            .as_ref()
            .map(|s| s.parse::<StencilMount>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid stencil-mount: {}", e))?;

        Ok(CliOverrides {
            channel_width_mm: self.channel_width,
            channel_depth_mm: self.channel_depth,
            via_style,
            channel_profile,
            channel_floor_width_mm: self.channel_floor_width,
            taper_slice_height_mm: self.taper_slice_height,
            cone_angle_deg: self.cone_angle,
            throat_height_mm: self.throat_height,
            min_rim_mm: self.min_rim,
            eyelet_style,
            eyelet_diameter_mm: self.eyelet_diameter,
            indent_depth_mm: self.indent_depth,
            pad_hole_diameter_mm: self.pad_hole_diameter,
            substrate_thickness_mm: self.substrate_thickness,
            scale_factor: self.scale,
            output_format,
            output_dir: self.output_dir.clone(),
            generate_pad_holes: if self.no_pad_holes { Some(false) } else { None },
            generate_pad_lands: if self.no_pad_lands { Some(false) } else { None },
            generate_via_indents: if self.no_via_indents { Some(false) } else { None },
            generate_stencil: if self.stencil { Some(true) } else { None },
            stencil_plating_bus: if self.plating_bus { Some(true) } else { None },
            stencil_mount,
            mode,
        })
    }
}

/// Open a 3D model file in the system's default viewer or in view3dscene.
///
/// Tries view3dscene first (if available), then falls back to the system default:
/// - Linux: xdg-open
/// - macOS: open
/// - Windows: start
fn open_viewer(file_path: &PathBuf) {
    // Try view3dscene first
    if Command::new("view3dscene")
        .arg(file_path)
        .spawn()
        .is_ok()
    {
        println!("🔍 Opening in view3dscene...");
        return;
    }

    // Fall back to system default
    let cmd = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", &file_path.display().to_string()])
            .spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open")
            .arg(file_path)
            .spawn()
    } else {
        // Linux and other Unix-like systems
        Command::new("xdg-open")
            .arg(file_path)
            .spawn()
    };

    match cmd {
        Ok(_) => println!("🔍 Opening in default viewer..."),
        Err(e) => eprintln!("⚠️  Could not open viewer: {}", e),
    }
}

/// Validates a generated mesh and reports anything that would make a slicer
/// misbehave.
///
/// This is deliberately a warning rather than a hard failure: a slightly
/// imperfect mesh is often still printable, and refusing to write the file
/// would leave the user with nothing to inspect. But it must never be silent —
/// the defects it looks for (an unclosed surface, inconsistent winding, zero
/// enclosed volume) are exactly the ones that look fine in a 3D preview and
/// then slice into a corked hole or a featureless plaque.
fn check_mesh(mesh: &geometry::Mesh3D, what: &str, verbose: bool) {
    let report = geometry::validate_mesh(mesh);
    if let Some(problems) = report.problems() {
        eprintln!("\n⚠️  {what}: mesh validation failed — {}", report.summary());
        for line in problems.lines() {
            eprintln!("   {line}");
        }
        eprintln!("   Inspect the STL before printing; the slicer may not show this.");
    } else if verbose {
        println!("   ✓ {what} validated: {}", report.summary());
    }
}

fn cli_main() -> Result<()> {
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

    // The mode has to be known before the file is read: it selects the preset
    // that the file layers on top of.
    let overrides = args.to_overrides()?;
    let mut config = Config::load(config_path, overrides.mode)?;

    // Step 2: Apply command-line overrides
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
        if (scale_factor - 1.0).abs() > 0.001 {
            println!("   Scale factor: {:.2}x (manual override)", scale_factor);
        } else {
            println!("   Scale factor: 1.00x (true size — component spacing preserved)");
        }
    }

    let _pcb_data = if (scale_factor - 1.0).abs() > 0.001 {
        pcb_data.scale(scale_factor)
    } else {
        pcb_data
    };

    // Step 6: Generate 3D geometry
    if args.verbose {
        println!("\n🔧 Generating 3D geometry...");
    }

    // If a specific side was requested, generate a modified PcbData with the
    // opposite-layer traces removed so the model contains only the requested side.
    let (geometry_pcb, stem_suffix) = if let Some(side_raw) = args.side.as_deref() {
        match side_raw.to_lowercase().as_str() {
            "top" => {
                if args.verbose {
                    println!("   Generating top side only...");
                }
                let pcb::PcbData { outline, traces_fcu, traces_bcu: _traces_bcu, arc_traces, vias, pads, footprints, cutouts } = _pcb_data;
                (
                    pcb::PcbData {
                        outline,
                        traces_fcu,
                        traces_bcu: Vec::new(),
                        arc_traces,
                        vias,
                        pads,
                        footprints,
                        cutouts,
                    },
                    "_top",
                )
            }
            "bottom" => {
                if args.verbose {
                    println!("   Generating bottom side only...");
                }
                let pcb::PcbData { outline, traces_fcu: _traces_fcu, traces_bcu, arc_traces, vias, pads, footprints, cutouts } = _pcb_data;
                (
                    pcb::PcbData {
                        outline,
                        traces_fcu: Vec::new(),
                        traces_bcu,
                        arc_traces,
                        vias,
                        pads,
                        footprints,
                        cutouts,
                    },
                    "_bottom",
                )
            }
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid side: '{}'. Use 'top' or 'bottom', or omit for combined model.",
                    other
                ));
            }
        }
    } else {
        (_pcb_data, "")
    };

    let mesh = geometry::generate_model(&geometry_pcb, &config)
        .context("Failed to generate 3D geometry")?;

    if args.verbose {
        println!("   Generated {} triangles", mesh.triangle_count());
    }

    // Step 6b: Check the mesh is actually a closed solid before writing it.
    // A gap here is invisible in the preview but decides what the slicer does:
    // an open surface is what turns a through-hole into a corked one, or the
    // whole board into a flat plaque. Cheaper to catch now than after a print.
    check_mesh(&mesh, "substrate", args.verbose);

    // Step 7: Export to files
    if args.verbose {
        println!("\n💾 Exporting to {} format...", config.output_format);
    }

    let stem = format!(
        "{}{}",
        args.input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("board"),
        stem_suffix
    );

    let mut written = export::export(&mesh, &geometry_pcb, &args.input, &stem, &config)
        .context("Failed to export 3D model")?;

    // Step 7b: Optionally generate the snap-on paint stencil(s) + plating bus.
    if config.generate_stencil {
        if args.verbose {
            println!("\n🩹 Generating snap-on paint stencil(s)...");
        }
        let out_dir = std::path::Path::new(&config.output_dir);
        for (layer, suffix) in [
            (pcb::CopperLayer::FCu, "_stencil_top"),
            (pcb::CopperLayer::BCu, "_stencil_bottom"),
        ] {
            if let Some(smesh) = geometry::generate_stencil(&geometry_pcb, &config, layer)
                .context("Failed to generate stencil geometry")?
            {
                check_mesh(&smesh, &format!("stencil{}", suffix), args.verbose);
                let path = out_dir.join(format!("{}{}.stl", stem, suffix));
                export::stl::write(&smesh, &path).context("Failed to write stencil STL")?;
                if args.verbose {
                    println!(
                        "   {} ({} triangles)",
                        path.display(),
                        smesh.triangle_count()
                    );
                }
                written.push(path);
            }
        }

        // Ring mount: also emit the single reusable L-section clamp ring.
        if config.stencil_mount == StencilMount::Ring {
            let ring = geometry::generate_clamp_ring(&geometry_pcb, &config)
                .context("Failed to generate clamp ring geometry")?;
            check_mesh(&ring, "clamp ring", args.verbose);
            let path = out_dir.join(format!("{}_stencil_ring.stl", stem));
            export::stl::write(&ring, &path).context("Failed to write clamp ring STL")?;
            if args.verbose {
                println!("   {} ({} triangles)", path.display(), ring.triangle_count());
            }
            written.push(path);
        }
    }

    println!("\n✅ Done! Generated:");
    for f in &written {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let label = if name.ends_with("_guide.html") {
            "  📋 Build guide (assembly + continuity + 3D)"
        } else if name.ends_with("_stencil_ring.stl") {
            "  ⭕ Stencil clamp ring (reusable, snaps to PCB)"
        } else if name.contains("_stencil_") {
            "  🩹 Paint stencil (print contact-face down)"
        } else if f.extension().map(|e| e == "html").unwrap_or(false) {
            "  🌐 Preview (interactive)"
        } else {
            "  📦"
        };
        println!("{} {}", label, f.display());
    }

    // Open viewer if requested (prefer HTML preview)
    if args.view {
        let html_file = written.iter().find(|f| f.extension().map(|e| e == "html").unwrap_or(false));
        if let Some(preview) = html_file {
            open_viewer(preview);
        } else if let Some(first) = written.first() {
            open_viewer(first);
        }
    }

    Ok(())
}
