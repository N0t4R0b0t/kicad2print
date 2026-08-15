// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Configuration management for `kicad2print`.
//!
//! This module handles loading settings from TOML config files and merging them with
//! command-line argument overrides. The configuration drives the geometry generation
//! and output options.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The mode presets, compiled into the binary.
///
/// `--mode` used to apply a short hardcoded list of geometry defaults that was
/// meant to mirror these files but drifted from them badly: the preset carried
/// two dozen settings the hardcoded block never touched, so `--mode
/// electrolysis` and `--config presets/electrolysis.toml` quietly produced
/// different boards while the documentation claimed they were equivalent.
///
/// Embedding rather than reading `presets/` at runtime is deliberate — an
/// installed binary has no such directory beside it, so a runtime read would
/// work from a source checkout and silently fall back to bare defaults
/// everywhere else.
const PRESET_COPPER_WIRE: &str = include_str!("../presets/copper-wire.toml");
const PRESET_ELECTROLYSIS: &str = include_str!("../presets/electrolysis.toml");

/// Construction mode — selects default geometry and assembly guide style.
///
/// Each mode is *defined by* its preset TOML in `presets/`, which is compiled
/// into the binary and applied as the baseline for that mode. Individual
/// settings can still be overridden via TOML or CLI.
///
/// # Values
/// - `"copper-wire"` — lay 30 AWG wire into grooves (wider channels, standard wire-laying guide)
/// - `"electrolysis"` — electroplate copper into grooves (narrower channels, plating guide)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    CopperWire,
    Electrolysis,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::CopperWire => write!(f, "copper-wire"),
            Mode::Electrolysis => write!(f, "electrolysis"),
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "copper-wire" | "wire" => Ok(Mode::CopperWire),
            "electrolysis" | "electro" => Ok(Mode::Electrolysis),
            other => Err(format!("Unknown mode: '{}'. Use 'copper-wire' or 'electrolysis'", other)),
        }
    }
}

impl Mode {
    /// The preset TOML that defines this mode's baseline settings.
    pub fn preset_toml(self) -> &'static str {
        match self {
            Mode::CopperWire => PRESET_COPPER_WIRE,
            Mode::Electrolysis => PRESET_ELECTROLYSIS,
        }
    }
}

/// How the paint stencil registers onto the PCB.
///
/// - `"lip"` — an integral perimeter lip on the plate wraps the board edge.
/// - `"ring"` — flat plates (printable contact-face-down for a smooth masking
///   finish) plus a separate, reusable L-section clamp ring that snaps onto the
///   PCB and folds a lip over the plate to wedge it against the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StencilMount {
    Lip,
    #[default]
    Ring,
}

impl std::fmt::Display for StencilMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StencilMount::Lip => write!(f, "lip"),
            StencilMount::Ring => write!(f, "ring"),
        }
    }
}

impl std::str::FromStr for StencilMount {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "lip" | "integral" => Ok(StencilMount::Lip),
            "ring" | "clamp" => Ok(StencilMount::Ring),
            other => Err(format!("Unknown stencil mount: '{}'. Use 'lip' or 'ring'", other)),
        }
    }
}

/// Cross-section profile of the trace grooves.
///
/// Electroplating a *rectangular* groove tends to leave the middle starved:
/// current density is highest at the top corners and lowest at the floor
/// centre, so copper grows inward from the side walls and can bridge over the
/// opening — sealing a void — before the floor is covered. Sloping the walls
/// removes the re-entrant corner, so the deposit fills from the bottom up and
/// the cross-section stays self-similar as it grows.
///
/// A sloped groove also prints better on an FDM machine: each layer's opening
/// is a different width, so the slicer lays progressively wider perimeters
/// instead of fighting a constant narrow gap between two vertical walls. The
/// gain is largest on the *bottom* face, where a rectangular groove has to be
/// closed off by bridging a flat ceiling and a sloped one does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChannelProfile {
    /// Vertical walls, flat floor — the original square-U groove.
    #[default]
    Rect,
    /// Sloped walls down to a flat floor of `channel_floor_width_mm`.
    /// Keeps some floor area (and so some copper cross-section) while still
    /// removing the corner that starves the centre.
    Trapezoid,
    /// Sloped walls that converge, ignoring `channel_floor_width_mm`.
    ///
    /// The model narrows to well under a nozzle width; where the groove gets
    /// too narrow to extrude, the slicer stops opening it and prints solid, so
    /// the printed groove ends in a naturally truncated point rather than the
    /// deliberate flat shelf a trapezoid carves. Best plating behaviour, at the
    /// cost of roughly half the copper cross-section for a given width and
    /// depth — prefer `Trapezoid` where current capacity matters.
    Vee,
}

impl std::fmt::Display for ChannelProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelProfile::Rect => write!(f, "rect"),
            ChannelProfile::Trapezoid => write!(f, "trapezoid"),
            ChannelProfile::Vee => write!(f, "vee"),
        }
    }
}

impl std::str::FromStr for ChannelProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rect" | "rectangular" | "square" | "u" => Ok(ChannelProfile::Rect),
            "trapezoid" | "trap" => Ok(ChannelProfile::Trapezoid),
            "vee" | "v" => Ok(ChannelProfile::Vee),
            other => Err(format!(
                "Unknown channel profile: '{}'. Use 'rect', 'trapezoid', or 'vee'",
                other
            )),
        }
    }
}

/// How a through-hole's barrel is shaped.
///
/// The problem this exists to solve: on a two-layer board the front and back
/// copper networks are otherwise completely separate, and a straight bore
/// through 2 mm of plastic cannot be reached with a brush, so seed paint never
/// coats it. That leaves a pressed-in metal eyelet as the only conductor
/// between layers — fiddly to install, and its flange has to be trimmed
/// wherever pins are close together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViaStyle {
    /// A plain straight bore. Layer-to-layer continuity needs an eyelet or a
    /// soldered wire stitch.
    #[default]
    Straight,
    /// A countersink from each face meeting at a short straight throat.
    ///
    /// Every point of the barrel is then in line of sight from one face or the
    /// other, so seed paint can actually be applied and the plating grows from
    /// both mouths toward the middle. The bottom cone doubles as a solder cup,
    /// which is what makes a top-side trace solderable from underneath.
    ///
    /// The cost is board area: cone mouths are much wider than the bore, so on
    /// fine pitches they get automatically shrunk to keep clear of neighbouring
    /// copper on other nets.
    Cone,
}

impl std::fmt::Display for ViaStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ViaStyle::Straight => write!(f, "straight"),
            ViaStyle::Cone => write!(f, "cone"),
        }
    }
}

impl std::str::FromStr for ViaStyle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "straight" | "bore" | "hole" => Ok(ViaStyle::Straight),
            "cone" | "double-cone" | "countersink" => Ok(ViaStyle::Cone),
            other => Err(format!("Unknown via style: '{}'. Use 'straight' or 'cone'", other)),
        }
    }
}

/// Specifies how via eyelets are represented in the 3D model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EyeletStyle {
    /// Full through-holes for inserting copper eyelets
    /// Useful if you have a drill press and can drill after printing.
    Hole,
    /// Shallow indented dimples on top and bottom faces
    /// Faster to print and easier assembly (no drilling needed).
    Indent,
}

impl std::fmt::Display for EyeletStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EyeletStyle::Hole => write!(f, "hole"),
            EyeletStyle::Indent => write!(f, "indent"),
        }
    }
}

impl std::str::FromStr for EyeletStyle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "hole" => Ok(EyeletStyle::Hole),
            "indent" => Ok(EyeletStyle::Indent),
            other => Err(format!("Unknown eyelet style: '{}'. Use 'hole' or 'indent'", other)),
        }
    }
}

/// Output file format(s) to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// STL (Stereolithography) format - widely supported by 3D slicers
    Stl,
    /// 3MF format - modern format with better color/material support
    ThreeM,
    /// Generate both STL and 3MF formats
    Both,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Stl => write!(f, "stl"),
            OutputFormat::ThreeM => write!(f, "3mf"),
            OutputFormat::Both => write!(f, "both"),
        }
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stl" => Ok(OutputFormat::Stl),
            "3mf" => Ok(OutputFormat::ThreeM),
            "both" => Ok(OutputFormat::Both),
            other => Err(format!("Unknown format: '{}'. Use 'stl', '3mf', or 'both'", other)),
        }
    }
}

/// One step in the assembly guide.
///
/// Each step names a group of components (by reference designator) and provides
/// an optional instruction string shown beside the board graphic.
///
/// # Example (TOML)
/// ```toml
/// [[assembly_steps]]
/// name = "Install resistors"
/// components = ["R1", "R2", "R3"]
/// instruction = "Insert resistors into the board. Bend leads flush to substrate."
///
/// [[assembly_steps]]
/// name = "Lay front-copper wires"
/// wire_layer = "F.Cu"
/// instruction = "Lay 30 AWG wire into each highlighted groove on the top surface."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssemblyStep {
    /// Short name displayed as the step heading
    pub name: String,
    /// Reference designators of components highlighted in this step (e.g. ["R1", "R2"])
    #[serde(default)]
    pub components: Vec<String>,
    /// Highlight all traces on this layer ("F.Cu" or "B.Cu")
    #[serde(default)]
    pub wire_layer: Option<String>,
    /// Human-readable instruction text shown in the guide
    #[serde(default)]
    pub instruction: String,
}

/// Main configuration struct for the entire application.
///
/// All values have sensible defaults that work well for typical PCB designs.
/// Values can be customized via TOML config file or command-line overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Width of the groove channels that will hold the copper wire traces.
    ///
    /// The wire traces laid into these channels should be approximately this wide.
    /// Default: 1.2 mm (good for 30 AWG Kynar wire)
    #[serde(default = "default_channel_width")]
    pub channel_width_mm: f64,

    /// Depth of the groove channels below the substrate surface.
    ///
    /// A deeper channel makes the wire sit lower in the groove, providing better
    /// mechanical support. Default: 0.5 mm (enough to hold wire securely)
    #[serde(default = "default_channel_depth")]
    pub channel_depth_mm: f64,

    /// Cross-section profile of the trace grooves — see `ChannelProfile`.
    ///
    /// Default: "rect" (the original square-U groove), so existing designs
    /// regenerate unchanged. Switch to "trapezoid" or "vee" if electroplating
    /// leaves the centre of your traces unfilled.
    #[serde(default = "default_channel_profile")]
    pub channel_profile: ChannelProfile,

    /// Width of the groove *floor* when `channel_profile = "trapezoid"`.
    ///
    /// The groove opening stays at `channel_width_mm`; this narrows only the
    /// bottom, so board area and the stencil are unaffected. Wall slope is
    /// derived from this together with `channel_depth_mm`, rather than being
    /// configured separately — with the opening width and depth already fixed,
    /// specifying both a floor width and an angle would be contradictory.
    ///
    /// Ignored for "rect" (floor = opening) and "vee" (floor = 0, clamped up
    /// to one nozzle width). Default: 0.4 mm — one 0.4 mm nozzle track.
    #[serde(default = "default_channel_floor_width")]
    pub channel_floor_width_mm: f64,

    /// Height of each step in a tapered groove or cone, in millimeters.
    ///
    /// Sloped walls are built as a stack of thin constant-width bands rather
    /// than a true ramp. Set this to your slicer's layer height and the printed
    /// result is identical to a smooth slope, because the print is quantised to
    /// the same steps anyway.
    ///
    /// Smaller values mean more polygon boolean operations — the slowest and
    /// least robust part of the pipeline — so the band count is capped
    /// internally. Default: 0.2 mm.
    #[serde(default = "default_taper_slice_height")]
    pub taper_slice_height_mm: f64,

    /// Barrel shape for every through-hole — see `ViaStyle`.
    ///
    /// Default: "straight", so existing designs regenerate unchanged. Use
    /// "cone" for eyelet-free plated through-holes.
    #[serde(default = "default_via_style")]
    pub via_style: ViaStyle,

    /// Wall angle of the countersinks, in degrees from the board surface.
    ///
    /// 45° is both the standard countersink angle and the steepest overhang an
    /// FDM printer holds without support — which matters only for the *bottom*
    /// cone, since that one narrows as it rises and so is the overhanging one.
    /// A larger angle means a steeper, narrower cone that eats less board area
    /// but is harder to get paint into. Default: 45.
    #[serde(default = "default_cone_angle")]
    pub cone_angle_deg: f64,

    /// Height of the straight section where the two cones meet, in millimeters.
    ///
    /// Letting the cones meet at a knife edge gives a fragile throat that
    /// prints badly and leaves the component lead nothing to locate against. A
    /// short parallel section fixes both. Default: 0.4 mm.
    #[serde(default = "default_throat_height")]
    pub throat_height_mm: f64,

    /// Minimum material to leave between a cone mouth and any foreign-net
    /// copper or the board edge, in millimeters.
    ///
    /// Cone mouths are far wider than the bore they surround — a 0.8 mm hole
    /// with 45° cones through a 2.2 mm board wants a ~3 mm crater on each face,
    /// which on 2.54 mm pin pitch would short to its neighbour. Mouths are
    /// shrunk automatically to preserve this clearance. Default: 0.3 mm.
    #[serde(default = "default_min_rim")]
    pub min_rim_mm: f64,

    /// Deprecated and inert — use `via_style` instead.
    ///
    /// This never affected the generated geometry: vias have always been cut as
    /// full through-holes, whichever value was set. Kept so existing config
    /// files still load, and warned about at runtime when set to "indent".
    #[serde(default = "default_eyelet_style")]
    pub eyelet_style: EyeletStyle,

    /// Diameter of the via holes or indent dimples.
    ///
    /// Should match the size of your copper eyelets (typically M0.9 or M1.3).
    /// Default: 1.5 mm (good for standard eyelets)
    #[serde(default = "default_eyelet_diameter")]
    pub eyelet_diameter_mm: f64,

    /// Deprecated and inert — there are no indent dimples. Kept so existing
    /// config files still load. Use `via_style = "cone"` for a countersink.
    #[serde(default = "default_indent_depth")]
    pub indent_depth_mm: f64,

    /// Minimum diameter of component pad through-holes.
    ///
    /// Each pad uses its own drill size from the KiCad design (preserving
    /// the correct hole size for each component — e.g. 1.0mm for D-Sub pins,
    /// 3.2mm for mounting holes, 0.8mm for IC pins).
    ///
    /// This config sets a MINIMUM hole diameter — used for pads that have
    /// missing or unrealistically small drill values.
    /// Default: 0.8 mm
    #[serde(default = "default_pad_hole_diameter")]
    pub pad_hole_diameter_mm: f64,

    /// Total thickness of the printed substrate (from bottom to top).
    ///
    /// The channels are routed inward from the top and bottom surfaces to this depth.
    /// Default: 3.0 mm (rigid without excessive printing time)
    #[serde(default = "default_substrate_thickness")]
    pub substrate_thickness_mm: f64,

    /// Manual scale factor to apply to the entire board.
    ///
    /// Default 0 means 1:1 (true size). Component hole spacing is preserved.
    /// Setting this to any other value scales the entire model uniformly —
    /// components will no longer fit at their original positions.
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f64,

    /// Which output format(s) to generate.
    ///
    /// Default: "stl" (STL is most widely supported by slicers)
    #[serde(default = "default_output_format")]
    pub output_format: OutputFormat,

    /// Directory where output files will be written.
    ///
    /// Default: "./output" (created if it doesn't exist)
    #[serde(default = "default_output_dir")]
    pub output_dir: String,

    /// Whether to generate through-holes for component pads.
    ///
    /// Default: true (generate pad holes for eyelets)
    /// Set to false for rapid prototyping where you'll solder directly to component legs.
    #[serde(default = "default_generate_pad_holes")]
    pub generate_pad_holes: bool,

    /// Deprecated and inert — there have never been via indent guides to skip.
    /// Kept so existing config files still load; warned about when set false.
    #[serde(default = "default_generate_via_indents")]
    pub generate_via_indents: bool,

    /// Whether to carve a shallow, pad-shaped indent (rect/circle/oval, matching
    /// the real KiCad pad land) at every pad, same depth as trace channels.
    /// Merged into the trace channel network so electroplating fills a properly
    /// shaped, solderable pad rather than just the lead's round drill hole.
    /// A pad's actual through-hole (if any) is unaffected by this flag — it is
    /// only ever generated when the pad has a real KiCad drill value.
    ///
    /// Default: true
    #[serde(default = "default_generate_pad_lands")]
    pub generate_pad_lands: bool,

    /// Whether to also generate a snap-on conductive-paint stencil plus a
    /// temporary plating bus (one stencil per copper side that has traces).
    ///
    /// The stencil registers over the substrate top via a perimeter snap-lip.
    /// Through-slots over every trace groove let conductive paint squeegee only
    /// into the channels (minimal cleanup). Extra slots form a temporary bus —
    /// a perimeter rail plus one stub to each electrically-isolated trace island
    /// — so the whole layer plates from one cathode contact. The bus bars sit
    /// proud on the flat substrate and are ground off after plating.
    /// Default: false (auto-enabled in electrolysis mode).
    #[serde(default = "default_generate_stencil")]
    pub generate_stencil: bool,

    /// Whether the stencil also carries the temporary **plating bus** — a
    /// perimeter rail plus one stub to each isolated trace (with tie-bars) that
    /// shorts every trace to a single cathode contact for electroplating.
    ///
    /// Off by default: the plain stencil masks just the traces and via/pad holes.
    /// Enable it (or pass `--plating-bus`) when you want kicad2print to build the
    /// interconnection for you rather than adding it yourself in KiCad.
    #[serde(default = "default_stencil_plating_bus")]
    pub stencil_plating_bus: bool,

    /// Thickness of the printed stencil plate (mm). Also sets the deposited
    /// thickness of the temporary bus bars that get ground off after plating.
    #[serde(default = "default_stencil_thickness")]
    pub stencil_thickness_mm: f64,

    /// Extra width added to each trace slot in the stencil, per side (mm).
    /// Eases paint flow into the groove and absorbs print tolerance.
    #[serde(default = "default_stencil_slot_clearance")]
    pub stencil_slot_clearance_mm: f64,

    /// Height of the perimeter snap-lip that wraps down over the board edge (mm).
    /// Only needs to be a shallow grip to keep the stencil located — it does not
    /// need to reach the full substrate thickness. Default: 1.2 mm.
    #[serde(default = "default_stencil_wall_height")]
    pub stencil_wall_height_mm: f64,

    /// Wall thickness of the perimeter snap-lip (mm).
    #[serde(default = "default_stencil_wall_thickness")]
    pub stencil_wall_thickness_mm: f64,

    /// Clearance between the snap-lip inner wall and the board edge (mm).
    /// Smaller = tighter snap fit; larger = looser. Tune to your printer.
    #[serde(default = "default_stencil_fit_clearance")]
    pub stencil_fit_clearance_mm: f64,

    /// Width of the temporary plating bus rail and its stubs (mm).
    #[serde(default = "default_bus_width")]
    pub bus_width_mm: f64,

    /// Distance the bus rail is inset from the board edge (mm).
    #[serde(default = "default_bus_inset")]
    pub bus_inset_mm: f64,

    /// Width of each tie-bar: a solid plate bridge that crosses the bus-rail ring
    /// so the plate inside the ring stays attached to the outer frame (otherwise
    /// it prints as a loose body and tears off when peeling the print). Each
    /// tie-bar interrupts the painted rail at that point. Default: 2.5 mm.
    #[serde(default = "default_bus_tie_width")]
    pub bus_tie_width_mm: f64,

    /// Number of tie-bars across the rail. 0 = auto (1 for small boards, 2 for
    /// larger ones). Note: with 2+ tie-bars the rail is split into that many
    /// separate arcs — clip the plating cathode to each arc. Use 1 to keep the
    /// rail a single conductor needing only one cathode clip. Default: 0 (auto).
    #[serde(default = "default_bus_tie_count")]
    pub bus_tie_count: u32,

    /// How the stencil registers onto the PCB: `"lip"` (integral perimeter lip on
    /// the plate) or `"ring"` (flat plates + a separate reusable L-section clamp
    /// ring). `ring` lets you print the contact face flat on the bed for a smooth
    /// masking finish. Default: `ring`.
    #[serde(default)]
    pub stencil_mount: StencilMount,

    /// (ring mount) How far the clamp ring's top lip covers the plate edge (mm).
    #[serde(default = "default_ring_lip_overlap")]
    pub ring_lip_overlap_mm: f64,

    /// (ring mount) Height of the clamp ring's top lip above the plate (mm).
    #[serde(default = "default_ring_lip_height")]
    pub ring_lip_height_mm: f64,

    /// Construction mode — selects assembly guide style and recommended geometry defaults.
    ///
    /// - `"copper-wire"`: lay physical wire into grooves; wide channels (1.2 mm)
    /// - `"electrolysis"`: electroplate copper into grooves; narrow channels (0.5 mm)
    ///
    /// This field does NOT override geometry settings already set in this config file.
    /// Use `--mode` on the CLI (or copy a preset from `presets/`) to get mode defaults.
    #[serde(default)]
    pub mode: Mode,

    /// Optional assembly guide steps.
    ///
    /// When non-empty, kicad2print generates an HTML assembly guide alongside the 3D model.
    /// Each step highlights specific components or wire layers on an SVG board view.
    /// If empty, a default guide is auto-generated based on the selected `mode`.
    #[serde(default)]
    pub assembly_steps: Vec<AssemblyStep>,
}

// Default value functions for serde
fn default_channel_width() -> f64 { 1.2 }
fn default_channel_depth() -> f64 { 0.5 }
fn default_channel_profile() -> ChannelProfile { ChannelProfile::Rect }
fn default_via_style() -> ViaStyle { ViaStyle::Straight }
fn default_cone_angle() -> f64 { 45.0 }
fn default_throat_height() -> f64 { 0.4 }
fn default_min_rim() -> f64 { 0.3 }
fn default_channel_floor_width() -> f64 { 0.4 }
fn default_taper_slice_height() -> f64 { 0.2 }
fn default_eyelet_style() -> EyeletStyle { EyeletStyle::Hole }
fn default_eyelet_diameter() -> f64 { 1.5 }
fn default_indent_depth() -> f64 { 0.3 }
fn default_pad_hole_diameter() -> f64 { 0.8 }
fn default_substrate_thickness() -> f64 { 3.0 }
fn default_scale_factor() -> f64 { 0.0 }
fn default_output_format() -> OutputFormat { OutputFormat::Stl }
fn default_output_dir() -> String { "./output".to_string() }
fn default_generate_pad_holes() -> bool { true }
fn default_generate_pad_lands() -> bool { true }
fn default_generate_via_indents() -> bool { true }
fn default_generate_stencil() -> bool { false }
fn default_stencil_plating_bus() -> bool { false }
fn default_stencil_thickness() -> f64 { 0.5 }
fn default_stencil_slot_clearance() -> f64 { 0.1 }
fn default_stencil_wall_height() -> f64 { 1.2 }
fn default_stencil_wall_thickness() -> f64 { 1.5 }
fn default_stencil_fit_clearance() -> f64 { 0.15 }
fn default_bus_width() -> f64 { 1.0 }
fn default_bus_inset() -> f64 { 1.5 }
fn default_bus_tie_width() -> f64 { 2.5 }
fn default_bus_tie_count() -> u32 { 0 }
fn default_ring_lip_overlap() -> f64 { 1.5 }
fn default_ring_lip_height() -> f64 { 1.0 }

impl Default for Config {
    fn default() -> Self {
        Config {
            channel_width_mm: default_channel_width(),
            channel_depth_mm: default_channel_depth(),
            channel_profile: default_channel_profile(),
            via_style: default_via_style(),
            cone_angle_deg: default_cone_angle(),
            throat_height_mm: default_throat_height(),
            min_rim_mm: default_min_rim(),
            channel_floor_width_mm: default_channel_floor_width(),
            taper_slice_height_mm: default_taper_slice_height(),
            eyelet_style: default_eyelet_style(),
            eyelet_diameter_mm: default_eyelet_diameter(),
            indent_depth_mm: default_indent_depth(),
            pad_hole_diameter_mm: default_pad_hole_diameter(),
            substrate_thickness_mm: default_substrate_thickness(),
            scale_factor: default_scale_factor(),
            output_format: default_output_format(),
            output_dir: default_output_dir(),
            generate_pad_holes: default_generate_pad_holes(),
            generate_pad_lands: default_generate_pad_lands(),
            generate_via_indents: default_generate_via_indents(),
            generate_stencil: default_generate_stencil(),
            stencil_plating_bus: default_stencil_plating_bus(),
            stencil_thickness_mm: default_stencil_thickness(),
            stencil_slot_clearance_mm: default_stencil_slot_clearance(),
            stencil_wall_height_mm: default_stencil_wall_height(),
            stencil_wall_thickness_mm: default_stencil_wall_thickness(),
            stencil_fit_clearance_mm: default_stencil_fit_clearance(),
            bus_width_mm: default_bus_width(),
            bus_inset_mm: default_bus_inset(),
            bus_tie_width_mm: default_bus_tie_width(),
            bus_tie_count: default_bus_tie_count(),
            stencil_mount: StencilMount::default(),
            ring_lip_overlap_mm: default_ring_lip_overlap(),
            ring_lip_height_mm: default_ring_lip_height(),
            mode: Mode::default(),
            assembly_steps: Vec::new(),
        }
    }
}

impl Config {
    /// Loads configuration from a TOML file, falling back to defaults for missing values.
    ///
    /// If the file doesn't exist, returns the default config (doesn't error).
    /// If the file exists but has invalid TOML, returns an error.
    ///
    /// # Arguments
    /// * `path` - Path to the TOML config file (e.g., "kicad2print.toml")
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // If file doesn't exist, just return defaults
        if !path.exists() {
            eprintln!("Note: config file {} not found, using defaults", path.display());
            return Ok(Config::default());
        }

        // Read and parse the file
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;

        let config = toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in config file {}", path.display()))?;

        Ok(config)
    }

    /// Loads the effective configuration, layering each source over the last:
    ///
    /// 1. built-in field defaults
    /// 2. the selected mode's preset (compiled in; see `Mode::preset_toml`)
    /// 3. the user's TOML file, if present
    /// 4. CLI flags (applied afterwards by `merge_cli_overrides`)
    ///
    /// Layering happens on the parsed TOML tables rather than on `Config`
    /// values, which is what makes "the user did not mention this key" and "the
    /// user set this key to the same value as the default" distinguishable. The
    /// previous approach compared each field against its default to guess
    /// whether it had been set, and so could not tell those apart.
    ///
    /// The mode is taken from `--mode` when given, otherwise from the user's
    /// file, otherwise the default.
    pub fn load<P: AsRef<Path>>(path: P, cli_mode: Option<Mode>) -> Result<Self> {
        let path = path.as_ref();

        let user_table: toml::Table = if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file {}", path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("Invalid TOML in config file {}", path.display()))?
        } else {
            if path.as_os_str() != "kicad2print.toml" {
                eprintln!("Note: config file {} not found, using defaults", path.display());
            }
            toml::Table::new()
        };

        let mode = cli_mode
            .or_else(|| {
                user_table
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Mode>().ok())
            })
            .unwrap_or_default();

        let mut table: toml::Table = toml::from_str(mode.preset_toml())
            .expect("built-in mode preset must be valid TOML");
        for (k, v) in user_table {
            table.insert(k, v);
        }

        let mut config: Config = table
            .try_into()
            .context("Failed to apply configuration (check key names and value types)")?;
        // `--mode` selected the baseline, so it also decides the final mode
        // regardless of what the user's file happens to say.
        config.mode = mode;
        Ok(config)
    }

    /// Merges CLI overrides into this config.
    ///
    /// Each `Option` in the input struct, if `Some`, overwrites the corresponding
    /// field in this config. `None` values are ignored.
    pub fn merge_cli_overrides(&mut self, overrides: &CliOverrides) {
        if overrides.channel_width_mm.is_some() {
            self.channel_width_mm = overrides.channel_width_mm.unwrap();
        }
        if overrides.channel_depth_mm.is_some() {
            self.channel_depth_mm = overrides.channel_depth_mm.unwrap();
        }
        if overrides.via_style.is_some() {
            self.via_style = overrides.via_style.unwrap();
        }
        if overrides.channel_profile.is_some() {
            self.channel_profile = overrides.channel_profile.unwrap();
        }
        if let Some(v) = overrides.channel_floor_width_mm {
            self.channel_floor_width_mm = v;
        }
        if let Some(v) = overrides.taper_slice_height_mm {
            self.taper_slice_height_mm = v;
        }
        if let Some(v) = overrides.cone_angle_deg {
            self.cone_angle_deg = v;
        }
        if let Some(v) = overrides.throat_height_mm {
            self.throat_height_mm = v;
        }
        if let Some(v) = overrides.min_rim_mm {
            self.min_rim_mm = v;
        }
        if overrides.eyelet_style.is_some() {
            self.eyelet_style = overrides.eyelet_style.unwrap();
        }
        if overrides.eyelet_diameter_mm.is_some() {
            self.eyelet_diameter_mm = overrides.eyelet_diameter_mm.unwrap();
        }
        if overrides.indent_depth_mm.is_some() {
            self.indent_depth_mm = overrides.indent_depth_mm.unwrap();
        }
        if overrides.pad_hole_diameter_mm.is_some() {
            self.pad_hole_diameter_mm = overrides.pad_hole_diameter_mm.unwrap();
        }
        if overrides.substrate_thickness_mm.is_some() {
            self.substrate_thickness_mm = overrides.substrate_thickness_mm.unwrap();
        }
        if overrides.scale_factor.is_some() {
            self.scale_factor = overrides.scale_factor.unwrap();
        }
        if overrides.output_format.is_some() {
            self.output_format = overrides.output_format.unwrap();
        }
        if overrides.output_dir.is_some() {
            self.output_dir = overrides.output_dir.as_ref().unwrap().clone();
        }
        if overrides.generate_pad_holes.is_some() {
            self.generate_pad_holes = overrides.generate_pad_holes.unwrap();
        }
        if overrides.generate_pad_lands.is_some() {
            self.generate_pad_lands = overrides.generate_pad_lands.unwrap();
        }
        if overrides.generate_via_indents.is_some() {
            self.generate_via_indents = overrides.generate_via_indents.unwrap();
        }
        if overrides.generate_stencil.is_some() {
            self.generate_stencil = overrides.generate_stencil.unwrap();
        }
        if overrides.stencil_plating_bus.is_some() {
            self.stencil_plating_bus = overrides.stencil_plating_bus.unwrap();
        }
        if let Some(mount) = overrides.stencil_mount {
            self.stencil_mount = mount;
        }
        // `mode` is deliberately absent here: it is applied earlier, by
        // `Config::load`, because it selects the preset baseline that
        // everything else layers on top of. Re-applying it at this point would
        // be too late to affect any of the geometry it is supposed to set.
    }

    /// Prints the current configuration to stdout.
    ///
    /// Useful for debugging to confirm which settings are being used.
    pub fn print_summary(&self) {
        println!("=== Configuration ===");
        println!("Mode:                {}", self.mode);
        println!("Channel width:       {:.2} mm", self.channel_width_mm);
        println!("Channel depth:       {:.2} mm", self.channel_depth_mm);
        println!("Eyelet style:        {}", self.eyelet_style);
        println!("Eyelet diameter:     {:.2} mm", self.eyelet_diameter_mm);
        println!("Indent depth:        {:.2} mm", self.indent_depth_mm);
        println!("Pad hole diameter:   {:.2} mm", self.pad_hole_diameter_mm);
        println!("Substrate thickness: {:.2} mm", self.substrate_thickness_mm);
        println!("Scale factor:        {}",
            if self.scale_factor > 0.0 { format!("{:.2}x (manual)", self.scale_factor) } else { "1.00x (true size)".to_string() }
        );
        println!("Output format:       {}", self.output_format);
        println!("Output directory:    {}", self.output_dir);
        println!("Generate pad holes:  {}", if self.generate_pad_holes { "yes" } else { "no" });
        println!("Generate pad lands:  {}", if self.generate_pad_lands { "yes" } else { "no" });
        println!("Generate via indents: {}", if self.generate_via_indents { "yes" } else { "no" });
        if self.generate_stencil {
            println!("Stencil mount:       {}", self.stencil_mount);
        }
    }
}

/// Command-line argument overrides.
///
/// This struct mirrors Config but with `Option` fields so that unspecified
/// arguments don't override config file values.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub via_style: Option<ViaStyle>,
    pub channel_profile: Option<ChannelProfile>,
    pub channel_floor_width_mm: Option<f64>,
    pub taper_slice_height_mm: Option<f64>,
    pub cone_angle_deg: Option<f64>,
    pub throat_height_mm: Option<f64>,
    pub min_rim_mm: Option<f64>,
    pub channel_width_mm: Option<f64>,
    pub channel_depth_mm: Option<f64>,
    pub eyelet_style: Option<EyeletStyle>,
    pub eyelet_diameter_mm: Option<f64>,
    pub indent_depth_mm: Option<f64>,
    pub pad_hole_diameter_mm: Option<f64>,
    pub substrate_thickness_mm: Option<f64>,
    pub scale_factor: Option<f64>,
    pub output_format: Option<OutputFormat>,
    pub output_dir: Option<String>,
    pub generate_pad_holes: Option<bool>,
    pub generate_pad_lands: Option<bool>,
    pub generate_via_indents: Option<bool>,
    pub generate_stencil: Option<bool>,
    pub stencil_plating_bus: Option<bool>,
    pub stencil_mount: Option<StencilMount>,
    pub mode: Option<Mode>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialised form, for comparing two configs field by field.
    fn as_table(c: &Config) -> toml::Table {
        toml::Table::try_from(c).expect("config serialises")
    }

    fn missing() -> &'static Path {
        Path::new("definitely-not-a-real-config-file.toml")
    }

    /// The regression this whole mechanism exists for. `--mode electrolysis`
    /// used to apply a hardcoded five-field block that was supposed to mirror
    /// `presets/electrolysis.toml` but had drifted from it by two dozen
    /// settings, so the two ways of selecting a mode produced different boards
    /// while the docs claimed they were equivalent.

    /// copper-wire is the default mode, so its preset must not introduce any
    /// difference from the built-in field defaults — otherwise running with no
    /// arguments would silently change behaviour the moment the preset became
    /// the baseline.
    #[test]
    fn copper_wire_preset_equals_the_builtin_defaults() {
        let defaults = as_table(&Config::default());
        let preset = as_table(&Config::load(missing(), Some(Mode::CopperWire)).expect("loads"));
        let diffs: Vec<_> = preset
            .iter()
            .filter(|(k, v)| defaults.get(*k) != Some(*v))
            .map(|(k, v)| format!("{k}: default={:?} preset={v:?}", defaults.get(k)))
            .collect();
        assert!(diffs.is_empty(), "copper-wire preset drifted from defaults:\n{}", diffs.join("\n"));
    }

    #[test]
    fn mode_flag_matches_its_preset_file_exactly() {
        for (mode, file) in [
            (Mode::Electrolysis, "presets/electrolysis.toml"),
            (Mode::CopperWire, "presets/copper-wire.toml"),
        ] {
            let from_flag = Config::load(missing(), Some(mode)).expect("mode preset loads");
            let from_file = Config::load(file, None).expect("preset file loads");
            assert_eq!(
                as_table(&from_flag),
                as_table(&from_file),
                "--mode {mode} and --config {file} must produce identical settings"
            );
        }
    }

    /// The specific settings that the old hardcoded block silently ignored.
    /// Their absence is what produced a board sliced at the wrong layer height.
    #[test]
    fn mode_preset_carries_the_geometry_settings_it_documents() {
        let c = Config::load(missing(), Some(Mode::Electrolysis)).expect("loads");
        assert_eq!(c.channel_width_mm, 0.7);
        assert_eq!(c.channel_depth_mm, 0.8);
        assert_eq!(c.substrate_thickness_mm, 2.2);
        assert!(c.generate_stencil, "electrolysis needs the seeding stencil");
        // None of these were reachable through --mode before.
        assert_eq!(c.channel_profile, ChannelProfile::Trapezoid);
        assert_eq!(c.taper_slice_height_mm, 0.12);
        assert_eq!(c.channel_floor_width_mm, 0.4);
    }

    #[test]
    fn copper_wire_is_the_default_mode() {
        let c = Config::load(missing(), None).expect("loads");
        assert_eq!(c.mode, Mode::CopperWire);
        assert_eq!(c.channel_width_mm, 1.2, "wire mode keeps the wide groove");
        assert_eq!(c.channel_profile, ChannelProfile::Rect, "wire seats flat");
    }

    #[test]
    fn a_user_file_overrides_the_mode_preset() {
        let dir = std::env::temp_dir().join(format!("k2p-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("user.toml");
        std::fs::write(&f, "channel_width_mm = 0.35\n").unwrap();

        let c = Config::load(&f, Some(Mode::Electrolysis)).expect("loads");
        assert_eq!(c.channel_width_mm, 0.35, "user file wins over the preset");
        assert_eq!(c.channel_depth_mm, 0.8, "untouched keys still come from the preset");
        assert_eq!(c.mode, Mode::Electrolysis, "--mode still selects the mode");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A user file that sets a key to the same value as the built-in default
    /// must still count as having set it. The old default-comparison approach
    /// could not distinguish that case from the key being absent.
    #[test]
    fn setting_a_key_to_its_default_value_still_overrides_the_preset() {
        let dir = std::env::temp_dir().join(format!("k2p-dflt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("user.toml");
        // 1.2 is the built-in default; electrolysis would otherwise force 0.7.
        std::fs::write(&f, "channel_width_mm = 1.2\n").unwrap();

        let c = Config::load(&f, Some(Mode::Electrolysis)).expect("loads");
        assert_eq!(
            c.channel_width_mm, 1.2,
            "explicitly asking for the default value must not be mistaken for silence"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cli_flags_beat_both_the_file_and_the_preset() {
        let mut c = Config::load(missing(), Some(Mode::Electrolysis)).expect("loads");
        c.merge_cli_overrides(&CliOverrides {
            channel_width_mm: Some(0.9),
            ..Default::default()
        });
        assert_eq!(c.channel_width_mm, 0.9);
        assert_eq!(c.channel_depth_mm, 0.8, "other preset values survive");
    }
}
