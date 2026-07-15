// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0

//! MCP server — exposes kicad2print as a Model Context Protocol tool.

use anyhow::Context as _;
use base64::{Engine, prelude::BASE64_STANDARD};
use rmcp::{
    ErrorData as McpError,
    ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    schemars,
    ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::process::Command;

use crate::{autoscale, config::Config, export, geometry, parser, parser::pcb_edit, render};

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WritePcbParams {
    /// Absolute or relative path where the file will be written
    pub path: String,
    /// Full KiCad S-expression content for the file
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConvertPcbParams {
    /// Absolute or relative path to the .kicad_pcb file
    pub input_path: String,
    /// Optional side to convert: "top" or "bottom". Omit to convert both sides
    /// together (combined model). When set, output files are suffixed with
    /// `_top` or `_bottom` (for example `boardname_top.stl`).
    #[schemars(default)]
    pub side: Option<String>,
    /// Output directory (default: "./output")
    #[schemars(default)]
    pub output_dir: Option<String>,
    /// Width of trace channels in mm (default: 0.6)
    #[schemars(default)]
    pub channel_width_mm: Option<f64>,
    /// Depth of trace channels in mm (default: 0.3)
    #[schemars(default)]
    pub channel_depth_mm: Option<f64>,
    /// Total substrate thickness in mm (default: 2.0)
    #[schemars(default)]
    pub substrate_thickness_mm: Option<f64>,
    /// Output format: "stl", "3mf", or "both" (default: "3mf")
    #[schemars(default)]
    pub output_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderPcbParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// View side: "top", "bottom", "front", "back", "left", "right" (default: "top")
    #[schemars(default)]
    pub side: Option<String>,
    /// Image width in pixels (default: 1200)
    #[schemars(default)]
    pub width: Option<u32>,
    /// Image height in pixels (default: 800)
    #[schemars(default)]
    pub height: Option<u32>,
    /// Render quality: "basic" or "high" (default: "high")
    #[schemars(default)]
    pub quality: Option<String>,
    /// Camera zoom factor (default: 1.5 — fits the board in frame)
    #[schemars(default)]
    pub zoom: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DrcParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchematicParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LibraryParams {
    /// Library name (e.g. "Connector_PinHeader_2.54mm") or absolute path to a .pretty directory
    pub library: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetFootprintParams {
    /// Library name (e.g. "Connector_PinHeader_2.54mm") or absolute path to a .pretty directory
    pub library: String,
    /// Footprint name without extension (e.g. "PinHeader_1x02_P2.54mm_Vertical")
    pub footprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NoParams {}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScanProjectParams {
    /// Path to a directory containing a KiCad project
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExportSvgParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Comma-separated layer names to export (e.g. "F.Cu,B.Cu,Edge.Cuts").
    /// Common layers: F.Cu, B.Cu, F.Silkscreen, B.Silkscreen, Edge.Cuts, F.Fab, B.Fab
    pub layers: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchFootprintParams {
    /// Search query — matched against library and footprint names (case-insensitive substring)
    pub query: String,
    /// Maximum number of results to return (default: 30)
    #[schemars(default)]
    pub max_results: Option<usize>,
    /// Optional path to a PCB or project file — enables discovery of project-local .pretty libraries
    #[schemars(default)]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetComponentParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator (e.g. "U1", "C3")
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReplaceFootprintParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator of the component to replace (e.g. "U1")
    pub reference: String,
    /// Library name or absolute path to a .pretty directory
    pub library: String,
    /// Footprint name without extension (e.g. "Arduino_Nano_Socket")
    pub footprint: String,
    /// Keep the existing position and rotation (default: true)
    #[schemars(default)]
    pub keep_position: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeleteComponentParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designators to remove (e.g. ["C1", "C2", "C3"])
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PadSpec {
    /// Pad number (e.g. "1", "A1")
    pub number: String,
    /// Pad type: "thru_hole" or "smd"
    pub pad_type: String,
    /// Pad shape: "circle", "rect", or "oval"
    pub shape: String,
    /// X position relative to footprint origin (mm)
    pub x: f64,
    /// Y position relative to footprint origin (mm)
    pub y: f64,
    /// Pad width (mm)
    pub size_x: f64,
    /// Pad height (mm)
    pub size_y: f64,
    /// Drill diameter for thru_hole pads (mm) — omit for SMD
    #[schemars(default)]
    pub drill: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GrepKicadParams {
    /// Absolute path to the KiCad file
    pub path: String,
    /// String to search for (case-sensitive substring match)
    pub query: String,
    /// Number of lines of context before and after each match (default: 3)
    #[schemars(default)]
    pub context_lines: Option<usize>,
    /// Maximum number of matches to return (default: 20)
    #[schemars(default)]
    pub max_matches: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadSectionParams {
    /// Absolute path to the KiCad file
    pub path: String,
    /// First line to return (1-based, default: 1)
    #[schemars(default)]
    pub offset: Option<usize>,
    /// Maximum number of lines to return (default: 300)
    #[schemars(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchKicadParams {
    /// Absolute path to the KiCad file to modify
    pub path: String,
    /// Exact string to find (must match exactly, including whitespace)
    pub old_string: String,
    /// Replacement string
    pub new_string: String,
    /// Replace all occurrences instead of only the first (default: false)
    #[schemars(default)]
    pub replace_all: Option<bool>,
    /// After patching a .kicad_sch file, render a preview image (default: true)
    #[schemars(default)]
    pub render_preview: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderSchematicParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Color theme name (default: KiCad default theme)
    #[schemars(default)]
    pub theme: Option<String>,
    /// Render in black and white (default: false)
    #[schemars(default)]
    pub black_and_white: Option<bool>,
    /// Image width in pixels (default: 2400)
    #[schemars(default)]
    pub width: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateFootprintParams {
    /// Absolute path to the .pretty library directory where the footprint will be saved
    pub library_path: String,
    /// Footprint name without extension (e.g. "MyConnector_1x04_P2.54mm")
    pub name: String,
    /// Short description of the footprint
    #[schemars(default)]
    pub description: Option<String>,
    /// Tags for searching (space-separated)
    #[schemars(default)]
    pub tags: Option<String>,
    /// Pad definitions
    pub pads: Vec<PadSpec>,
    /// Extra margin added around pad extents for the courtyard (mm, default: 0.25)
    #[schemars(default)]
    pub courtyard_margin: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveComponentParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator of the component to move (e.g. "U1")
    pub reference: String,
    /// New absolute X position in mm — omit to use dx instead
    #[schemars(default)]
    pub x: Option<f64>,
    /// New absolute Y position in mm — omit to use dy instead
    #[schemars(default)]
    pub y: Option<f64>,
    /// Relative X offset in mm (applied to current position when x is not given)
    #[schemars(default)]
    pub dx: Option<f64>,
    /// Relative Y offset in mm (applied to current position when y is not given)
    #[schemars(default)]
    pub dy: Option<f64>,
    /// New rotation in degrees — omit to keep existing rotation
    #[schemars(default)]
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddComponentParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Library name or absolute path to a .pretty directory
    pub library: String,
    /// Footprint name without extension
    pub footprint: String,
    /// Reference designator for the new component (e.g. "U3")
    pub reference: String,
    /// Value string for the new component (e.g. "100nF")
    pub value: String,
    /// X position in mm
    pub x: f64,
    /// Y position in mm
    pub y: f64,
    /// Rotation in degrees (default: 0)
    #[schemars(default)]
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErcParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExportFabParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Output directory for fabrication files (default: "./fab" next to the PCB file)
    #[schemars(default)]
    pub output_dir: Option<String>,
    /// Comma-separated copper layers to include in gerbers (default: "F.Cu,B.Cu,F.Mask,B.Mask,F.SilkS,B.SilkS,F.Paste,B.Paste,Edge.Cuts")
    #[schemars(default)]
    pub layers: Option<String>,
    /// Also generate a zip archive of the output files (default: true)
    #[schemars(default)]
    pub zip: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdatePcbFromSchParams {
    /// Absolute path to the .kicad_sch schematic file
    pub sch_path: String,
    /// Absolute path to the .kicad_pcb PCB file to update
    pub pcb_path: String,
    /// Dry run — report what would change without writing (default: false)
    #[schemars(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddWireParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Start X position in mm
    pub x1: f64,
    /// Start Y position in mm
    pub y1: f64,
    /// End X position in mm
    pub x2: f64,
    /// End Y position in mm
    pub y2: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddLabelParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Net name for the label
    pub text: String,
    /// X position in mm
    pub x: f64,
    /// Y position in mm
    pub y: f64,
    /// Rotation in degrees (default: 0)
    #[schemars(default)]
    pub rotation: Option<f64>,
    /// Label type: "local" or "global" (default: "local")
    #[schemars(default)]
    pub label_type: Option<String>,
    /// Shape for global labels: "bidirectional", "input", "output", "tri_state", "passive" (default: "bidirectional")
    #[schemars(default)]
    pub global_shape: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveLabelParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// The label text to find
    pub text: String,
    /// Current X position in mm (used to disambiguate)
    pub old_x: f64,
    /// Current Y position in mm (used to disambiguate)
    pub old_y: f64,
    /// New X position in mm
    pub new_x: f64,
    /// New Y position in mm
    pub new_y: f64,
    /// New rotation in degrees — if omitted, keeps existing rotation
    #[schemars(default)]
    pub new_rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReplaceSymbolParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Reference designator of the symbol to replace (e.g. "U2")
    pub reference: String,
    /// New symbol lib_id in "library:symbol" format (e.g. "ps2-serial-mouse-adapter:MAX3232_Module")
    pub new_lib_id: String,
    /// New footprint in "library:footprint" format (e.g. "ps2-serial-mouse-adapter:MAX3232_Module_8Pad")
    #[schemars(default)]
    pub new_footprint: Option<String>,
    /// New value string (default: keep existing)
    #[schemars(default)]
    pub new_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CleanupTracesParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Only remove segments on these layers (e.g. "F.Cu,B.Cu"); default: all copper layers
    #[schemars(default)]
    pub layers: Option<String>,
    /// Dry run — report what would be removed without writing (default: false)
    #[schemars(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FillZonesParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AutoroutePcbParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Maximum autorouter passes (default: 40)
    #[schemars(default)]
    pub max_passes: Option<u32>,
    /// Absolute path to the FreeRouting JAR (default: ~/.local/share/freerouting.jar)
    #[schemars(default)]
    pub freerouting_jar: Option<String>,
    /// Save the routed result back to the original file (default: true)
    #[schemars(default)]
    pub save: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CleanupDanglingWiresParams {
    /// Absolute path to the .kicad_sch schematic file
    pub path: String,
    /// Dry run — report what would be removed without writing (default: false)
    #[schemars(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetBoardOutlineParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetBoardOutlineParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Left edge of board in mm
    pub x_min: f64,
    /// Top edge of board in mm
    pub y_min: f64,
    /// Right edge of board in mm
    pub x_max: f64,
    /// Bottom edge of board in mm
    pub y_max: f64,
    /// Also update the first copper zone polygon to match (default: true)
    #[schemars(default)]
    pub update_zones: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddTraceParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Start X in mm
    pub x1: f64,
    /// Start Y in mm
    pub y1: f64,
    /// End X in mm
    pub x2: f64,
    /// End Y in mm
    pub y2: f64,
    /// Copper layer (e.g. "F.Cu", "B.Cu")
    pub layer: String,
    /// Trace width in mm (default: 0.25)
    #[schemars(default)]
    pub width: Option<f64>,
    /// Net name string (e.g. "GND", "VBUS"). Omit or leave empty for unconnected.
    #[schemars(default)]
    pub net: Option<String>,
    /// If true, run the same collision check as check_trace_clearance before writing.
    /// Any COLLISION aborts the write (no file change) and returns the report instead.
    /// Warnings never block. Default false (preserves prior always-write behavior).
    #[schemars(default)]
    pub check: Option<bool>,
    /// If true, write the trace even if the check reports collisions. Has no effect
    /// unless check is also true. Default false.
    #[schemars(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeleteGraphicParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Delete gr_text items containing this string (case-sensitive substring)
    #[schemars(default)]
    pub text_contains: Option<String>,
    /// Delete items on this layer (e.g. "F.SilkS", "Edge.Cuts") — combined with text_contains as AND filter
    #[schemars(default)]
    pub layer: Option<String>,
    /// Delete items with this exact tstamp UUID
    #[schemars(default)]
    pub tstamp: Option<String>,
    /// Also delete footprint blocks matching text_contains in their reference or value
    #[schemars(default)]
    pub include_footprints: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeleteTraceParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Delete segments belonging to this net name (exact match)
    #[schemars(default)]
    pub net: Option<String>,
    /// Delete segments on this copper layer (e.g. "F.Cu") — combined with other filters as AND
    #[schemars(default)]
    pub layer: Option<String>,
    /// Delete the single segment with this exact tstamp/uuid (substring match)
    #[schemars(default)]
    pub uuid: Option<String>,
    /// Region left X boundary in mm — all four of x1/y1/x2/y2 must be given together
    #[schemars(default)]
    pub x1: Option<f64>,
    /// Region top Y boundary in mm
    #[schemars(default)]
    pub y1: Option<f64>,
    /// Region right X boundary in mm
    #[schemars(default)]
    pub x2: Option<f64>,
    /// Region bottom Y boundary in mm
    #[schemars(default)]
    pub y2: Option<f64>,
    /// If true, report what WOULD be removed without writing anything. Recommended
    /// before a net/layer/region filter that could match more than expected —
    /// pairs with query_traces_in_region for discovery.
    #[schemars(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddGraphicParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Type of graphic: "text", "line", "rect", "circle" (default: "text")
    #[schemars(default)]
    pub graphic_type: Option<String>,
    /// Text content (for type "text")
    #[schemars(default)]
    pub text: Option<String>,
    /// X position / start X in mm
    pub x: f64,
    /// Y position / start Y in mm
    pub y: f64,
    /// End X in mm (for line/rect)
    #[schemars(default)]
    pub x2: Option<f64>,
    /// End Y in mm (for line/rect)
    #[schemars(default)]
    pub y2: Option<f64>,
    /// Layer (default: "F.SilkS")
    #[schemars(default)]
    pub layer: Option<String>,
    /// Font size in mm (for text, default: 1.0)
    #[schemars(default)]
    pub font_size: Option<f64>,
    /// Line/stroke width in mm (default: 0.12)
    #[schemars(default)]
    pub width: Option<f64>,
    /// Rotation in degrees (default: 0)
    #[schemars(default)]
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetPadPositionParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator (e.g. "U1")
    pub reference: String,
    /// Pad number to look up (e.g. "1", "A3") — if omitted returns all pads
    #[schemars(default)]
    pub pad: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveSymbolParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Reference designator of the symbol to move (e.g. "U1")
    pub reference: String,
    /// New X position in mm (schematic coordinates)
    pub x: f64,
    /// New Y position in mm (schematic coordinates)
    pub y: f64,
    /// New rotation in degrees (default: keep existing)
    #[schemars(default)]
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetPinPositionParams {
    /// Absolute path to the .kicad_sch file
    pub path: String,
    /// Reference designator (e.g. "U1")
    pub reference: String,
    /// Pin number to look up (e.g. "5") — if omitted returns all pins
    #[schemars(default)]
    pub pin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ListNetsParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryPadsInRegionParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Left X boundary in mm
    pub x1: f64,
    /// Top Y boundary in mm
    pub y1: f64,
    /// Right X boundary in mm
    pub x2: f64,
    /// Bottom Y boundary in mm
    pub y2: f64,
    /// Filter to pads on this copper layer (e.g. "F.Cu", "B.Cu") — omit for all layers
    #[schemars(default)]
    pub layer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryTracesInRegionParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Left X boundary in mm
    pub x1: f64,
    /// Top Y boundary in mm
    pub y1: f64,
    /// Right X boundary in mm
    pub x2: f64,
    /// Bottom Y boundary in mm
    pub y2: f64,
    /// Filter to traces on this copper layer (e.g. "F.Cu", "B.Cu") — omit for all layers
    #[schemars(default)]
    pub layer: Option<String>,
    /// Filter to traces on this net — omit for all nets
    #[schemars(default)]
    pub net: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckTraceClearanceParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Proposed trace start X in mm
    pub x1: f64,
    /// Proposed trace start Y in mm
    pub y1: f64,
    /// Proposed trace end X in mm
    pub x2: f64,
    /// Proposed trace end Y in mm
    pub y2: f64,
    /// Copper layer the trace would be on (e.g. "F.Cu", "B.Cu")
    pub layer: String,
    /// Trace width in mm (default: 0.25)
    #[schemars(default)]
    pub width: Option<f64>,
    /// Minimum required clearance from pad edges in mm (default: 0.1)
    #[schemars(default)]
    pub clearance: Option<f64>,
    /// Net name the proposed trace belongs to (e.g. "GND"). Segments already on
    /// this same net are allowed to touch/overlap (T-junctions, continuations)
    /// and are excluded from trace-vs-trace collision checks. Omit if the trace
    /// is unrouted/has no net yet — in that case it is still checked against
    /// other unrouted segments, since two unrouted traces touching is a real
    /// collision, not a legitimate junction.
    #[schemars(default)]
    pub net: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddPowerSymbolParams {
    /// Absolute path to the .kicad_sch schematic file
    pub path: String,
    /// Net name matching a KiCad power library symbol (e.g. "VBUS", "GND", "+5V")
    pub net_name: String,
    /// X position in schematic coordinates (mm)
    pub x: f64,
    /// Y position in schematic coordinates (mm)
    pub y: f64,
    /// Rotation in degrees (default: 0)
    #[schemars(default)]
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GetNetForPadParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator of the footprint (e.g. "U1", "JP1")
    pub reference: String,
    /// Pad number as a string (e.g. "1", "A3")
    pub pad_number: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VerifyConnectivityParams {
    /// Absolute path to the .kicad_pcb file
    pub path: String,
    /// Reference designator of the first pad (e.g. "U1")
    pub ref_a: String,
    /// Pad number of the first pad (e.g. "30")
    pub pad_a: String,
    /// Reference designator of the second pad (e.g. "JP1")
    pub ref_b: String,
    /// Pad number of the second pad (e.g. "1")
    pub pad_b: String,
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct KiCadServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    /// Candidate directories to search for footprint .pretty libraries
    fp_lib_dirs: Vec<PathBuf>,
    /// Per-canonical-path mutex so concurrent tool calls on the same file are serialized.
    /// Without this, two replace_footprint calls issued together both read the original
    /// file, splice independently, and the last write wins — losing the first edit.
    file_locks: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>>,
}

impl KiCadServer {
    pub fn new() -> Self {
        let fp_lib_dirs = footprint_library_search_dirs();
        Self {
            tool_router: Self::tool_router(),
            fp_lib_dirs,
            file_locks: Default::default(),
        }
    }

    /// Acquire an exclusive lock for `path` before read-modify-write operations.
    async fn lock_file(&self, path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let lock = {
            let mut map = self.file_locks.lock().await;
            map.entry(canonical)
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// Render the board at `path` and return a base64 PNG Content item, or None on failure.
    /// Render both top and bottom views of the board and return them as Content items.
    async fn render_board(&self, path: &str) -> Vec<Content> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Compute zoom from Edge.Cuts so the render always crops to the board boundary.
        // kicad-cli render at zoom=1.0 fits ~80mm of board width into 1200px.
        // We scale to fit the board with 10% padding.
        let zoom = if let Ok(content) = std::fs::read_to_string(path) {
            let (x1, y1, x2, y2) = parse_edge_cuts_bounds(&content);
            let board_w = (x2 - x1).abs().max(1.0);
            let board_h = (y2 - y1).abs().max(1.0);
            // empirical: at zoom=1.5 a 80×60mm board fills 1200×800px → 15px/mm
            let px_per_mm = 15.0_f64;
            let zoom_w = (1200.0 / px_per_mm) / board_w;
            let zoom_h = (800.0  / px_per_mm) / board_h;
            let z = zoom_w.min(zoom_h) * 0.90; // 10% padding
            format!("{:.3}", z.clamp(0.3, 10.0))
        } else {
            "1.5".to_string()
        };

        let mut contents = Vec::new();
        for side in &["top", "bottom"] {
            let out_path = std::env::temp_dir().join(format!("kicad_render_{ts}_{side}.png"));
            let ok = run_kicad_cli(&[
                "pcb", "render",
                "--output", out_path.to_str().unwrap_or("/tmp/render.png"),
                "--width", "1200", "--height", "800",
                "--side", side, "--quality", "basic",
                "--background", "opaque",
                "--zoom", &zoom,
                path,
            ]).await
                .map(|(_, _, code)| code == 0)
                .unwrap_or(false);

            if ok {
                if let Ok(bytes) = fs::read(&out_path).await {
                    let _ = fs::remove_file(&out_path).await;
                    contents.push(Content::text(format!("{} view:", side)));
                    contents.push(Content::image(BASE64_STANDARD.encode(&bytes), "image/png"));
                }
            }
        }
        contents
    }

    /// Export a schematic to SVG then convert to PNG via rsvg-convert; returns a PNG Content item.
    async fn render_schematic_png(&self, path: &str, theme: Option<&str>, bw: bool, width: u32) -> Option<Content> {
        let tmp_dir = std::env::temp_dir().join(format!(
            "kicad_sch_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let _ = fs::create_dir_all(&tmp_dir).await;

        let mut args = vec![
            "sch".to_string(), "export".to_string(), "svg".to_string(),
            "--output".to_string(), tmp_dir.to_str().unwrap_or("/tmp").to_string(),
        ];
        if let Some(t) = theme {
            args.push("--theme".to_string());
            args.push(t.to_string());
        }
        if bw { args.push("--black-and-white".to_string()); }
        args.push(path.to_string());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let ok = run_kicad_cli(&arg_refs).await
            .map(|(_, _, code)| code == 0)
            .unwrap_or(false);
        if !ok { return None; }

        // Find the exported SVG file
        let svg_path = {
            let mut found = None;
            if let Ok(mut rd) = fs::read_dir(&tmp_dir).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    let p = entry.path();
                    if p.extension().map(|e| e == "svg").unwrap_or(false) {
                        found = Some(p);
                        break;
                    }
                }
            }
            found?
        };

        // Convert SVG → PNG with rsvg-convert
        let png_path = tmp_dir.join("out.png");
        let conv = Command::new("rsvg-convert")
            .args([
                "-w", &width.to_string(),
                "-o", png_path.to_str().unwrap_or("/tmp/out.png"),
                svg_path.to_str().unwrap_or(""),
            ])
            .output()
            .await;

        let _ = fs::remove_file(&svg_path).await;

        if conv.map(|o| o.status.success()).unwrap_or(false) {
            if let Ok(bytes) = fs::read(&png_path).await {
                let _ = fs::remove_file(&png_path).await;
                let _ = fs::remove_dir(&tmp_dir).await;
                return Some(Content::image(BASE64_STANDARD.encode(&bytes), "image/png"));
            }
        }
        None
    }

    /// Resolve a library name or path to an absolute .pretty directory.
    fn resolve_library(&self, library: &str) -> Option<PathBuf> {
        let p = PathBuf::from(library);
        if p.is_absolute() && p.is_dir() {
            return Some(p);
        }
        for dir in &self.fp_lib_dirs {
            let candidate = dir.join(format!("{}.pretty", library));
            if candidate.is_dir() {
                return Some(candidate);
            }
            // Maybe user passed the bare dir name inside a search dir
            let candidate2 = dir.join(library);
            if candidate2.is_dir() {
                return Some(candidate2);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Footprint library helpers
// ---------------------------------------------------------------------------

fn footprint_library_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // User-local KiCad footprints (downloaded via KiCad PCM)
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/kicad/9.0/footprints"));
        dirs.push(PathBuf::from(&home).join(".local/share/kicad/8.0/footprints"));
    }
    // System-installed kicad-library package
    dirs.push(PathBuf::from("/usr/share/kicad/footprints"));
    dirs.push(PathBuf::from("/usr/local/share/kicad/footprints"));

    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

/// Collect all .pretty library dirs reachable from the search dirs, plus
/// any project-local .pretty dirs near `project_path`.
async fn collect_all_pretty_dirs(
    base_dirs: &[PathBuf],
    project_path: Option<&Path>,
) -> Vec<PathBuf> {
    let mut result = Vec::new();

    // System / user library dirs
    for base in base_dirs {
        if let Ok(mut rd) = fs::read_dir(base).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let p = entry.path();
                if p.is_dir() && p.extension().map(|e| e == "pretty").unwrap_or(false) {
                    result.push(p);
                }
            }
        }
    }

    // Project-local .pretty dirs (walk up 2 levels from pcb file)
    if let Some(proj) = project_path {
        for ancestor in proj.ancestors().take(3) {
            if let Ok(mut rd) = fs::read_dir(ancestor).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    let p = entry.path();
                    if p.is_dir() && p.extension().map(|e| e == "pretty").unwrap_or(false) {
                        if !result.contains(&p) {
                            result.push(p);
                        }
                    }
                }
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Shared CLI runner
// ---------------------------------------------------------------------------

async fn run_kicad_cli(args: &[&str]) -> Result<(String, String, i32), McpError> {
    let mut cmd = Command::new("kicad-cli");
    cmd.args(args);
    // Suppress X11/display errors — kicad-cli works headless
    cmd.env("DISPLAY", "");

    let out = cmd.output().await.map_err(|e| {
        McpError::internal_error(format!("Failed to spawn kicad-cli: {e}"), None)
    })?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out.status.code().unwrap_or(-1);
    Ok((stdout, stderr, code))
}

/// Upgrade a single .kicad_mod file to the current KiCad format by copying it
/// into a temporary .pretty library, running `kicad-cli fp upgrade`, and reading
/// back the result. Returns the upgraded S-expression content.
async fn upgrade_footprint_format(fp_path: &Path, fp_name: &str) -> anyhow::Result<String> {
    let tmp_lib = std::env::temp_dir().join(format!(
        "kicad_fp_upgrade_{}.pretty",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    fs::create_dir_all(&tmp_lib).await?;
    let tmp_fp = tmp_lib.join(format!("{}.kicad_mod", fp_name));
    fs::copy(fp_path, &tmp_fp).await?;

    let out = Command::new("kicad-cli")
        .args(["fp", "upgrade", "--force", tmp_lib.to_str().unwrap_or("")])
        .env("DISPLAY", "")
        .output()
        .await?;

    let content = if out.status.success() {
        fs::read_to_string(&tmp_fp).await?
    } else {
        // kicad-cli reported an error — fall back to the original
        fs::read_to_string(fp_path).await?
    };

    let _ = fs::remove_dir_all(&tmp_lib).await;
    Ok(content)
}

// ---------------------------------------------------------------------------
// Trace cleanup helpers
// ---------------------------------------------------------------------------

/// A parsed trace segment from a .kicad_pcb file.
struct TraceSegment {
    /// Byte range of the full segment line (including leading whitespace, excluding trailing newline)
    range: std::ops::Range<usize>,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    layer: String,
}

// ---------------------------------------------------------------------------
// PCB board outline / zone / graphics helpers
// ---------------------------------------------------------------------------

/// Iterate over all top-level S-expression blocks in `content` that open with `keyword`
/// (e.g. `"(gr_line "`, `"(segment "`, `"(zone "`).
///
/// "Top-level" means the line containing the opener is indented by exactly one
/// indent unit — two spaces (KiCad 6/7) or one tab (KiCad 9/10). This avoids
/// matching the same keyword when it appears nested inside another block.
///
/// Calls `f(block_start_byte, block_end_byte)` for each match.
fn for_each_top_level<F>(content: &str, keyword: &str, mut f: F)
where
    F: FnMut(usize, usize),
{
    let mut pos = 0;
    while let Some(rel) = content[pos..].find(keyword) {
        let kw_start = pos + rel;
        let line_start = content[..kw_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &content[line_start..kw_start];
        if prefix == "  " || prefix == "\t" {
            let end = pcb_edit::block_end(content, kw_start);
            f(kw_start, end);
            pos = end;
        } else {
            pos = kw_start + 1;
        }
    }
}

/// Parse all Edge.Cuts gr_line start/end coordinates and return the bounding box.
fn parse_edge_cuts_bounds(content: &str) -> (f64, f64, f64, f64) {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();

    for kw in &["(gr_line ", "(gr_rect "] {
        for_each_top_level(content, kw, |start, end| {
            let block = &content[start..end];
            if block.contains("\"Edge.Cuts\"") || block.contains("Edge.Cuts") {
                for tag in &["(start ", "(end "] {
                    if let Some(p) = block.find(tag) {
                        let rest = &block[p + tag.len()..];
                        let mut parts = rest.split_whitespace();
                        if let (Some(sx), Some(sy)) = (parts.next(), parts.next()) {
                            if let (Ok(x), Ok(y)) = (sx.parse::<f64>(),
                                                      sy.trim_end_matches(')').parse::<f64>()) {
                                xs.push(x);
                                ys.push(y);
                            }
                        }
                    }
                }
            }
        });
    }

    if xs.is_empty() {
        return (0.0, 0.0, 100.0, 100.0);
    }
    let x_min = xs.iter().cloned().fold(f64::MAX, f64::min);
    let x_max = xs.iter().cloned().fold(f64::MIN, f64::max);
    let y_min = ys.iter().cloned().fold(f64::MAX, f64::min);
    let y_max = ys.iter().cloned().fold(f64::MIN, f64::max);
    (x_min, y_min, x_max, y_max)
}

/// Remove all Edge.Cuts gr_line and gr_rect top-level blocks from PCB content.
fn remove_edge_cuts_lines(content: &str) -> String {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for kw in &["(gr_line ", "(gr_rect "] {
        for_each_top_level(content, kw, |start, end| {
            let block = &content[start..end];
            if block.contains("\"Edge.Cuts\"") || block.contains("Edge.Cuts") {
                ranges.push(start..end);
            }
        });
    }
    ranges.sort_by(|a, b| b.start.cmp(&a.start));
    let mut result = content.to_string();
    for range in ranges {
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') { range.end + 1 } else { range.end };
        let start = if range.start > 0 && result.as_bytes().get(range.start - 1) == Some(&b'\n') {
            range.start - 1
        } else { range.start };
        result.drain(start..end.min(result.len()));
    }
    result
}

/// Update the first zone's polygon to match the given bounding box.
/// Update ALL zone polygons whose bounding box approximately matches `old_bounds` (±10%).
/// Zones whose polygon doesn't resemble the old board outline (local pours) are left alone.
fn update_all_zone_polygons(
    content: &str,
    (ox1, oy1, ox2, oy2): (f64, f64, f64, f64),
    (nx1, ny1, nx2, ny2): (f64, f64, f64, f64),
) -> String {
    let old_w = (ox2 - ox1).abs();
    let old_h = (oy2 - oy1).abs();

    // Collect all zones whose (polygon ...) bbox is within 20% of the old board outline.
    let mut replacements: Vec<(usize, usize)> = Vec::new(); // (poly_abs_start, poly_end)

    let mut pos = 0;
    while pos < content.len() {
        // Find next top-level "(zone "
        let Some(rel) = content[pos..].find("(zone ") else { break };
        let zone_start = pos + rel;
        let line_start = content[..zone_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &content[line_start..zone_start];
        if !(prefix == "  " || prefix == "\t") {
            pos = zone_start + 1;
            continue;
        }

        let zone_end = pcb_edit::block_end(content, zone_start);
        let zone_block = &content[zone_start..zone_end];

        if let Some(poly_rel) = zone_block.find("(polygon") {
            let poly_abs = zone_start + poly_rel;
            let poly_end = pcb_edit::block_end(content, poly_abs);
            let poly_block = &content[poly_abs..poly_end];

            // Parse all (xy X Y) points and compute bbox
            let mut xs: Vec<f64> = Vec::new();
            let mut ys: Vec<f64> = Vec::new();
            let mut scan = 0;
            while let Some(xyr) = poly_block[scan..].find("(xy ") {
                let after = &poly_block[scan + xyr + 4..];
                let mut parts = after.split_whitespace();
                if let (Some(sx), Some(sy)) = (parts.next(), parts.next()) {
                    if let (Ok(x), Ok(y)) = (sx.parse::<f64>(),
                                              sy.trim_end_matches(')').parse::<f64>()) {
                        xs.push(x); ys.push(y);
                    }
                }
                scan += xyr + 1;
            }

            if !xs.is_empty() {
                let pxmin = xs.iter().cloned().fold(f64::MAX, f64::min);
                let pxmax = xs.iter().cloned().fold(f64::MIN, f64::max);
                let pymin = ys.iter().cloned().fold(f64::MAX, f64::min);
                let pymax = ys.iter().cloned().fold(f64::MIN, f64::max);
                let pw = (pxmax - pxmin).abs();
                let ph = (pymax - pymin).abs();

                // Match if this polygon's size is within 20% of the old board outline
                let w_match = old_w < 0.01 || (pw - old_w).abs() / old_w.max(0.01) < 0.20;
                let h_match = old_h < 0.01 || (ph - old_h).abs() / old_h.max(0.01) < 0.20;
                if w_match && h_match {
                    replacements.push((poly_abs, poly_end));
                }
            }
        }
        pos = zone_end;
    }

    // Apply replacements in reverse order so byte offsets stay valid
    let new_poly = format!(
        "(polygon\n      (pts\n        (xy {nx1} {ny1})\n        (xy {nx2} {ny1})\n        (xy {nx2} {ny2})\n        (xy {nx1} {ny2})\n        (xy {nx1} {ny1})\n      )\n    )"
    );
    let mut result = content.to_string();
    for (start, end) in replacements.into_iter().rev() {
        result.replace_range(start..end, &new_poly);
    }
    result
}

/// Remove PCB graphic elements matching the given filters. Returns (new_content, count_removed).
fn remove_matching_graphics(
    content: &str,
    text_contains: Option<&str>,
    layer: Option<&str>,
    tstamp: Option<&str>,
    include_footprints: bool,
) -> (String, usize) {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();

    // Check gr_text, gr_line, gr_rect, gr_circle blocks (handles both space and tab indent)
    for kw in &["(gr_text ", "(gr_line ", "(gr_rect ", "(gr_circle "] {
        for_each_top_level(content, kw, |start, end| {
            let block = &content[start..end];
            let text_match = text_contains.map(|t| block.contains(t)).unwrap_or(true);
            let layer_match = layer.map(|l| block.contains(l)).unwrap_or(true);
            let tstamp_match = tstamp.map(|ts| block.contains(ts)).unwrap_or(true);
            if text_match && layer_match && tstamp_match {
                ranges.push(start..end);
            }
        });
    }

    // Optionally check footprint blocks
    if include_footprints {
        if let Some(tc) = text_contains {
            let blocks = pcb_edit::find_footprint_blocks(content);
            for (_, range) in &blocks {
                let block = &content[range.clone()];
                let layer_match = layer.map(|l| block.contains(l)).unwrap_or(true);
                let tstamp_match = tstamp.map(|ts| block.contains(ts)).unwrap_or(true);
                if (block.contains(tc)) && layer_match && tstamp_match {
                    ranges.push(range.clone());
                }
            }
        }
    }

    let count = ranges.len();
    ranges.sort_by(|a, b| b.start.cmp(&a.start));
    let mut result = content.to_string();
    for range in ranges {
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') { range.end + 1 } else { range.end };
        let start = if range.start > 0 && result.as_bytes().get(range.start - 1) == Some(&b'\n') {
            range.start - 1
        } else {
            range.start
        };
        result.drain(start..end.min(result.len()));
    }
    (result, count)
}

/// Find `(segment ...)` blocks matching the given filters (all provided filters
/// combine as AND). Returns each match's byte range plus a short human-readable
/// description (net/layer/endpoints), so a caller can preview matches (dry_run)
/// without necessarily removing them.
fn find_matching_traces(
    content: &str,
    net: Option<&str>,
    layer: Option<&str>,
    uuid: Option<&str>,
    region: Option<(f64, f64, f64, f64)>,
) -> Vec<(std::ops::Range<usize>, String)> {
    let mut matches = Vec::new();
    for_each_top_level(content, "(segment", |start, end| {
        let block = &content[start..end];
        let seg_net = parse_quoted_field(block, "(net \"").unwrap_or_default();
        let seg_layer = parse_quoted_field(block, "(layer \"");

        let net_match = net.map(|n| seg_net == n).unwrap_or(true);
        let layer_match = layer.map(|l| seg_layer.as_deref() == Some(l)).unwrap_or(true);
        let uuid_match = uuid.map(|u| block.contains(u)).unwrap_or(true);
        let region_match = region.map(|(xmin, ymin, xmax, ymax)| {
            if let (Some((x1, y1)), Some((x2, y2))) = (parse_xy_field(block, "(start "), parse_xy_field(block, "(end ")) {
                let (sxmin, sxmax) = (x1.min(x2), x1.max(x2));
                let (symin, symax) = (y1.min(y2), y1.max(y2));
                sxmin <= xmax && sxmax >= xmin && symin <= ymax && symax >= ymin
            } else {
                false
            }
        }).unwrap_or(true);

        if net_match && layer_match && uuid_match && region_match {
            let (x1, y1) = parse_xy_field(block, "(start ").unwrap_or((0.0, 0.0));
            let (x2, y2) = parse_xy_field(block, "(end ").unwrap_or((0.0, 0.0));
            let net_display = if seg_net.is_empty() { "<unrouted>" } else { &seg_net };
            let desc = format!(
                "net={net_display} layer={}  ({x1:.3},{y1:.3})→({x2:.3},{y2:.3})",
                seg_layer.as_deref().unwrap_or("?"),
            );
            matches.push((start..end, desc));
        }
    });
    matches
}

/// Remove the given byte ranges from `content` (descending order to keep earlier
/// ranges valid), trimming one adjacent newline per range — same splice logic
/// as `remove_matching_graphics`.
fn splice_out_ranges(content: &str, mut ranges: Vec<std::ops::Range<usize>>) -> String {
    ranges.sort_by(|a, b| b.start.cmp(&a.start));
    let mut result = content.to_string();
    for range in ranges {
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') { range.end + 1 } else { range.end };
        let start = if range.start > 0 && result.as_bytes().get(range.start - 1) == Some(&b'\n') {
            range.start - 1
        } else {
            range.start
        };
        result.drain(start..end.min(result.len()));
    }
    result
}

/// Extract absolute pad positions from a footprint block, accounting for footprint rotation.
/// Returns `(pad_number, global_x, global_y, layer)` for each pad in a footprint block.
fn extract_pad_positions(block: &str) -> Vec<(String, f64, f64, String)> {
    let (fp_x, fp_y, fp_rot) = pcb_edit::extract_at(block).unwrap_or((0.0, 0.0, 0.0));
    let rot_rad = fp_rot.to_radians();
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();

    let mut pads = Vec::new();
    let mut pos = 0;

    while let Some(rel) = block[pos..].find("(pad \"") {
        let pad_start = pos + rel;
        let line_start = block[..pad_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &block[line_start..pad_start];
        if !prefix.chars().all(|c| c == ' ' || c == '\t') {
            pos = pad_start + 1;
            continue;
        }

        let pad_end = pcb_edit::block_end(block, pad_start);
        let pad_block = &block[pad_start..pad_end];

        let pad_num = {
            let after = &pad_block[6..]; // skip (pad "
            after.find('"').map(|e| after[..e].to_string()).unwrap_or_default()
        };

        // Extract layer: (layers "F.Cu" ...) or (layers "*.Cu" ...)
        let layer = {
            let marker = "(layers \"";
            if let Some(lp) = pad_block.find(marker) {
                let after = &pad_block[lp + marker.len()..];
                after.find('"').map(|e| after[..e].to_string()).unwrap_or_else(|| "?".to_string())
            } else {
                "?".to_string()
            }
        };

        if let Some(at_pos) = pad_block.find("(at ") {
            let after = &pad_block[at_pos + 4..];
            let mut parts = after.split_whitespace();
            if let (Some(sx), Some(sy)) = (parts.next(), parts.next()) {
                let sy_clean = sy.trim_end_matches(')');
                if let (Ok(lx), Ok(ly)) = (sx.parse::<f64>(), sy_clean.parse::<f64>()) {
                    let gx = fp_x + lx * cos_r - ly * sin_r;
                    let gy = fp_y + lx * sin_r + ly * cos_r;
                    pads.push((pad_num, gx, gy, layer));
                }
            }
        }
        pos = pad_end;
    }
    pads
}

// ---------------------------------------------------------------------------
// Schematic symbol placement / pin position helpers
// ---------------------------------------------------------------------------

/// Remove dangling wire segments from schematic content.
/// A wire is dangling if either endpoint doesn't touch any other wire endpoint,
/// pin position, junction, label, or no_connect marker.
/// Returns (new_content, count_removed).
fn remove_dangling_wires(content: &str) -> (String, usize) {
    // Collect all wire endpoints
    struct Wire { range: std::ops::Range<usize>, x1: f64, y1: f64, x2: f64, y2: f64 }

    let coord_k = |v: f64| (v * 10_000.0).round() as i64;
    let pt_k = |x: f64, y: f64| (coord_k(x), coord_k(y));

    let mut wires: Vec<Wire> = Vec::new();
    // for_each_top_level matches both single-line and multi-line `(wire ...)` forms
    // and both 2-space/tab indentation (see find_symbol_blocks for the same fix).
    for_each_top_level(content, "(wire", |start, end| {
        let block = &content[start..end];

        // Parse (pts (xy X1 Y1) (xy X2 Y2))
        let mut pts = block.split("(xy ").skip(1);
        let parse_pt = |s: &str| -> Option<(f64, f64)> {
            let mut parts = s.split_whitespace();
            let x = parts.next()?.parse::<f64>().ok()?;
            let y = parts.next()?.trim_end_matches(')').parse::<f64>().ok()?;
            Some((x, y))
        };
        if let (Some(p1), Some(p2)) = (pts.next().and_then(parse_pt), pts.next().and_then(parse_pt)) {
            wires.push(Wire { range: start..end, x1: p1.0, y1: p1.1, x2: p2.0, y2: p2.1 });
        }
    });

    // Build set of all "anchored" points: pin positions, label positions, junctions, no_connects
    let mut anchors: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();

    // Pin positions from all symbol instances
    for reference in find_symbol_blocks(content).keys() {
        for (_, _, px, py) in compute_pin_positions(content, reference) {
            anchors.insert(pt_k(px, py));
        }
    }

    // Labels: (label "X" (at X Y ...) and (global_label "X" ... (at X Y ...)
    for kw in &["(label ", "(global_label ", "(no_connect ", "(junction "] {
        let mut pos = 0;
        while let Some(rel) = content[pos..].find(kw) {
            let start = pos + rel;
            if let Some(at_p) = content[start..].find("(at ") {
                let after = &content[start + at_p + 4..];
                let mut parts = after.split_whitespace();
                if let (Some(sx), Some(sy)) = (parts.next(), parts.next()) {
                    if let (Ok(x), Ok(y)) = (sx.parse::<f64>(), sy.trim_end_matches(')').parse::<f64>()) {
                        anchors.insert(pt_k(x, y));
                    }
                }
            }
            pos = start + 1;
        }
    }

    // Count how many wire endpoints share each coordinate (wire-to-wire connections)
    let mut endpoint_count: std::collections::HashMap<(i64, i64), usize> = std::collections::HashMap::new();
    for w in &wires {
        *endpoint_count.entry(pt_k(w.x1, w.y1)).or_insert(0) += 1;
        *endpoint_count.entry(pt_k(w.x2, w.y2)).or_insert(0) += 1;
    }

    // A wire endpoint is connected if: it's in anchors OR it appears in more than one wire
    let is_connected = |x: f64, y: f64| -> bool {
        let k = pt_k(x, y);
        anchors.contains(&k) || endpoint_count.get(&k).copied().unwrap_or(0) > 1
    };

    // Remove wires where BOTH endpoints are disconnected (truly dangling/isolated)
    let mut to_remove: Vec<std::ops::Range<usize>> = Vec::new();
    for w in &wires {
        if !is_connected(w.x1, w.y1) && !is_connected(w.x2, w.y2) {
            to_remove.push(w.range.clone());
        }
    }

    let count = to_remove.len();
    to_remove.sort_by(|a, b| b.start.cmp(&a.start));
    let mut result = content.to_string();
    for range in to_remove {
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') { range.end + 1 } else { range.end };
        let start = if range.start > 0 && result.as_bytes().get(range.start - 1) == Some(&b'\n') {
            range.start - 1
        } else { range.start };
        result.drain(start..end.min(result.len()));
    }
    (result, count)
}

/// Find a symbol instance block in a schematic by reference designator.
/// Returns the byte range of the block (not including the leading newline).
fn find_sch_symbol_by_ref(content: &str, reference: &str) -> Option<std::ops::Range<usize>> {
    find_symbol_blocks(content).remove(reference)
}

/// Replace the (at X Y ROT) in a schematic symbol instance block.
fn sch_replace_at(block: &str, new_x: f64, new_y: f64, rotation: Option<f64>) -> String {
    // The instance's own placement is always the first top-level "(at " in the
    // block — it comes right after (lib_id ...), before any property's own nested
    // (at ...). Works for both the single-line and multi-line symbol forms (the
    // instance's `(at ...)` doesn't have to share a line with `(lib_id ...)`).
    let at_needle = "(at ";
    if let Some(at_start) = block.find(at_needle) {
        let at_content_start = at_start + at_needle.len();
        // Find the closing ) of the (at ...) node
        let mut depth = 1i32;
        let mut end_rel = 0;
        for (i, ch) in block[at_content_start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => { depth -= 1; if depth == 0 { end_rel = i; break; } }
                _ => {}
            }
        }
        let at_end = at_content_start + end_rel + 1;

        // Parse existing rotation from old at content
        let old_at = &block[at_start..at_end];
        let existing_rot = {
            let inner = &old_at[4..]; // skip "(at "
            let parts: Vec<&str> = inner.split_whitespace().collect();
            if parts.len() >= 3 {
                parts[2].trim_end_matches(')').parse::<f64>().unwrap_or(0.0)
            } else { 0.0 }
        };
        let rot = rotation.unwrap_or(existing_rot);
        let new_at = if rot.abs() < 0.001 {
            format!("(at {} {})", new_x, new_y)
        } else {
            format!("(at {} {} {})", new_x, new_y, rot)
        };
        format!("{}{}{}", &block[..at_start], new_at, &block[at_end..])
    } else {
        block.to_string()
    }
}

/// Compute absolute schematic canvas positions of all pins for a symbol instance.
/// Returns Vec<(pin_number, pin_name, canvas_x, canvas_y)>.
fn compute_pin_positions(content: &str, reference: &str) -> Vec<(String, String, f64, f64)> {
    compute_pin_positions_inner(content, reference).unwrap_or_default()
}

fn compute_pin_positions_inner(content: &str, reference: &str) -> Option<Vec<(String, String, f64, f64)>> {
    // 1. Find the symbol instance to get lib_id and placement
    let instance_range = find_sch_symbol_by_ref(content, reference)?;
    let instance_block = &content[instance_range];

    // Extract lib_id
    let lib_id = {
        let marker = "(lib_id \"";
        let pos = instance_block.find(marker)?;
        let after = &instance_block[pos + marker.len()..];
        after[..after.find('"')?].to_string()
    };

    // Extract placement (at X Y ROT) — the instance's own placement, always the
    // first top-level "(at " in the block (see sch_replace_at); not necessarily on
    // the same physical line as `(symbol`/`(lib_id ...)` in the multi-line form.
    let (inst_x, inst_y, inst_rot): (f64, f64, f64) = {
        if let Some(at_pos) = instance_block.find("(at ") {
            let after = &instance_block[at_pos + 4..];
            let parts: Vec<&str> = after.split_whitespace().collect();
            let x: f64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let y: f64 = parts.get(1).and_then(|s| s.trim_end_matches(')').parse().ok()).unwrap_or(0.0);
            let rot: f64 = parts.get(2).and_then(|s| s.trim_end_matches(')').parse().ok()).unwrap_or(0.0);
            (x, y, rot)
        } else {
            (0.0, 0.0, 0.0)
        }
    };

    // 2. Find the symbol definition in lib_symbols
    let lib_sym_start = content.find("(lib_symbols")?;
    let lib_sym_end = pcb_edit::block_end(content, lib_sym_start);
    let lib_sym_section = &content[lib_sym_start..lib_sym_end];

    // Find matching symbol definition: (symbol "LIB:NAME" ...
    let sym_marker = format!("(symbol \"{}\"", lib_id);
    let sym_pos = lib_sym_section.find(&sym_marker)?;
    let sym_end = pcb_edit::block_end(lib_sym_section, sym_pos);
    let sym_def = &lib_sym_section[sym_pos..sym_end];

    // 3. Extract pin definitions: (pin TYPE STYLE (at PX PY ROT) (length L) (name "N") (number "NUM"))
    let rot_rad = inst_rot.to_radians();
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();

    let mut results = Vec::new();
    let mut pos = 0;
    // Use whitespace-agnostic search: find "(pin " anywhere in the sym_def,
    // but only accept it when preceded by only whitespace on the same line.
    while let Some(rel) = sym_def[pos..].find("(pin ") {
        let pin_start = pos + rel;
        // Verify it's on its own line (preceded only by whitespace)
        let line_start = sym_def[..pin_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &sym_def[line_start..pin_start];
        if !prefix.chars().all(|c| c == ' ' || c == '\t') {
            pos = pin_start + 1;
            continue;
        }
        let end = pcb_edit::block_end(sym_def, pin_start);
        let pin_block = &sym_def[pin_start..end];

        // Parse pin at: (at PX PY ROT) — first (at after the pin keyword
        let (px, py) = if let Some(at_p) = pin_block.find("(at ") {
            let after = &pin_block[at_p + 4..];
            let parts: Vec<&str> = after.split_whitespace().collect();
            let x = parts.first().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let y = parts.get(1).and_then(|s| s.trim_end_matches(')').parse::<f64>().ok()).unwrap_or(0.0);
            (x, y)
        } else { (0.0, 0.0) };

        // Pin endpoint is at (px, py) plus length along pin direction — for label attachment
        // we return the pin base position (where the wire connects)
        // Apply instance rotation + translation
        let canvas_x = inst_x + px * cos_r - py * sin_r;
        let canvas_y = inst_y + px * sin_r + py * cos_r;

        // Extract name and number
        let name = extract_quoted_after_kw(pin_block, "(name \"");
        let number = extract_quoted_after_kw(pin_block, "(number \"");

        if !number.is_empty() {
            results.push((number, name, canvas_x, canvas_y));
        }
        pos = end;
    }
    Some(results)
}

fn extract_quoted_after_kw(block: &str, keyword: &str) -> String {
    if let Some(p) = block.find(keyword) {
        let after = &block[p + keyword.len()..];
        if let Some(end) = after.find('"') {
            return after[..end].to_string();
        }
    }
    String::new()
}

/// Rounds a coordinate to a 5-micron bucket for exact-hash spatial matching
/// (verify_connectivity's BFS, cleanup_traces' pad-proximity check). 5 microns
/// is a 20x safety margin below any realistic KiCad clearance (>=0.1mm/100um),
/// so genuinely distinct copper can never collapse into the same bucket, while
/// comfortably absorbing the sub-micron float drift between independently
/// computed rotated-footprint pad centers and trace endpoints that used to
/// cause false DISCONNECTED reports at the previous 0.1-micron precision.
fn coord_key(v: f64) -> i64 {
    (v * 200.0).round() as i64
}

fn point_key(x: f64, y: f64) -> (i64, i64) {
    (coord_key(x), coord_key(y))
}

/// Parse all top-level segment blocks from PCB content.
/// Returns a list of TraceSegment values with byte ranges.
fn parse_segments(content: &str) -> Vec<TraceSegment> {
    let mut segments = Vec::new();
    for_each_top_level(content, "(segment ", |start, end| {
        let block = &content[start..end];
        if let (Some((x1, y1)), Some((x2, y2))) = (
            extract_xy(block, "(start "),
            extract_xy(block, "(end "),
        ) {
            let layer = extract_quoted_field(block, "layer").unwrap_or_default();
            segments.push(TraceSegment { range: start..end, x1, y1, x2, y2, layer });
        }
    });
    segments
}

/// Extract `(field X Y)` — returns (X, Y) or None.
fn extract_xy(block: &str, prefix: &str) -> Option<(f64, f64)> {
    let pos = block.find(prefix)?;
    let after = &block[pos + prefix.len()..];
    let close = after.find(')')?;
    let nums: Vec<f64> = after[..close]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums.as_slice() {
        [x, y, ..] => Some((*x, *y)),
        _ => None,
    }
}

/// Extract the value of a quoted field like `(layer "F.Cu")`.
fn extract_quoted_field(block: &str, field: &str) -> Option<String> {
    let marker = format!("({} \"", field);
    let pos = block.find(&marker)?;
    let after = &block[pos + marker.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Collect all pad global positions from footprint blocks.
/// Uses the simplified approach: just collect footprint-level (at X Y) positions.
/// This is enough to prevent deleting traces that connect to component pads.
fn collect_pad_positions(content: &str) -> Vec<(f64, f64)> {
    use pcb_edit::{find_footprint_blocks, extract_at};
    let mut positions = Vec::new();

    for (_ref, range) in find_footprint_blocks(content) {
        let fp_block = &content[range.clone()];

        // Footprint origin
        let (fx, fy, frot) = extract_at(fp_block).unwrap_or((0.0, 0.0, 0.0));
        let frot_rad = frot.to_radians();
        let cos_r = frot_rad.cos();
        let sin_r = frot_rad.sin();

        // Iterate over (pad ...) blocks within the footprint
        let mut pad_search = 0usize;
        while let Some(rel) = fp_block[pad_search..].find("(pad ") {
            let pad_start = pad_search + rel;
            let pad_end = pcb_edit::block_end(fp_block, pad_start);
            let pad_block = &fp_block[pad_start..pad_end];

            // Pad-local (at X Y [ROT])
            if let Some(at_pos) = pad_block.find("(at ") {
                let after = &pad_block[at_pos + 4..];
                if let Some(close) = after.find(')') {
                    let nums: Vec<f64> = after[..close]
                        .split_whitespace()
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    if nums.len() >= 2 {
                        let (px, py) = (nums[0], nums[1]);
                        // Rotate pad position by footprint rotation
                        let gx = fx + px * cos_r - py * sin_r;
                        let gy = fy + px * sin_r + py * cos_r;
                        positions.push((gx, gy));
                    }
                }
            }

            pad_search = pad_end;
        }

        // Also add the footprint origin itself as an anchor
        positions.push((fx, fy));
    }

    positions
}

/// Remove orphaned trace segments from the given PCB content.
/// Returns (new_content, removed_count, layer_counts).
fn remove_orphaned_segments(
    content: &str,
    layer_filter: Option<&[&str]>,
) -> (String, usize, std::collections::HashMap<String, usize>) {
    use std::collections::{HashMap, HashSet};

    let segments = parse_segments(content);
    let pad_positions = collect_pad_positions(content);

    // Build point → segment indices map
    let mut point_to_segs: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, seg) in segments.iter().enumerate() {
        point_to_segs.entry(point_key(seg.x1, seg.y1)).or_default().push(i);
        point_to_segs.entry(point_key(seg.x2, seg.y2)).or_default().push(i);
    }

    // Build a set of pad point keys (with tolerance: we snap to 0.01mm = 100 in key units)
    // We just store each pad's key and check proximity
    let pad_keys: HashSet<(i64, i64)> = pad_positions.iter()
        .map(|(x, y)| point_key(*x, *y))
        .collect();

    fn is_near_pad(pk: (i64, i64), pad_keys: &HashSet<(i64, i64)>) -> bool {
        // Tolerance: 0.01mm = 2 units (coord_key is now 200 units/mm, not 10,000 —
        // this MUST move in lockstep with coord_key's scale factor, or this
        // "100 units" would silently become 0.5mm instead of the intended
        // 0.01mm, a 50x loosening that could make cleanup_traces treat
        // genuinely separate pads as touching).
        let tol = 2i64;
        for &(px, py) in pad_keys {
            if (pk.0 - px).abs() <= tol && (pk.1 - py).abs() <= tol {
                return true;
            }
        }
        false
    }

    // Union-find for connected components
    let n = segments.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        if parent[x] != x { parent[x] = find(parent, parent[x]); }
        parent[x]
    }

    fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb { parent[ra] = rb; }
    }

    // Union segments that share an endpoint
    for segs in point_to_segs.values() {
        for window in segs.windows(2) {
            union(&mut parent, window[0], window[1]);
        }
    }

    // Determine which components are "live" (touch a pad)
    let mut live_roots: HashSet<usize> = HashSet::new();
    for (i, seg) in segments.iter().enumerate() {
        let pk1 = point_key(seg.x1, seg.y1);
        let pk2 = point_key(seg.x2, seg.y2);
        if is_near_pad(pk1, &pad_keys) || is_near_pad(pk2, &pad_keys) {
            live_roots.insert(find(&mut parent, i));
        }
    }

    // Propagate liveness via BFS (in case union-find roots aren't consistent after purity)
    // Actually union-find already groups all connected segments — we just need to check root
    // Mark all components that have at least one live segment as live
    // (The above loop already inserts the root of any segment with a pad-touching endpoint)

    // Build per-component live set
    let mut component_live: HashSet<usize> = HashSet::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        if live_roots.contains(&root) {
            component_live.insert(root);
        }
    }

    // Collect segments to remove
    let mut to_remove: Vec<usize> = Vec::new();
    let mut layer_counts: HashMap<String, usize> = HashMap::new();

    for (i, seg) in segments.iter().enumerate() {
        // Apply layer filter
        if let Some(layers) = layer_filter {
            if !layers.contains(&seg.layer.as_str()) {
                continue;
            }
        }

        let root = find(&mut parent, i);
        if !component_live.contains(&root) {
            to_remove.push(i);
            *layer_counts.entry(seg.layer.clone()).or_insert(0) += 1;
        }
    }

    if to_remove.is_empty() {
        return (content.to_string(), 0, layer_counts);
    }

    // Remove in reverse order by byte offset
    let mut ranges: Vec<std::ops::Range<usize>> = to_remove.iter()
        .map(|&i| segments[i].range.clone())
        .collect();
    ranges.sort_by(|a, b| b.start.cmp(&a.start));

    let mut result = content.to_string();
    for range in &ranges {
        // Also eat the trailing newline if present
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') {
            range.end + 1
        } else {
            range.end
        };
        result.drain(range.start..end);
    }

    (result, to_remove.len(), layer_counts)
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl KiCadServer {
    // ---- File I/O ----------------------------------------------------------

    /// Read the raw S-expression content of a KiCad file (.kicad_pcb or .kicad_sch).
    /// Returns the full text so it can be inspected or used as a base for edits.
    /// Supports optional offset/limit for pagination of large files.
    #[tool(description = "Read a KiCad file (.kicad_pcb or .kicad_sch) and return its raw S-expression content. Use offset/limit to paginate large files.")]
    async fn read_kicad_file(
        &self,
        params: Parameters<ReadSectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&params.0.path);
        let raw = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        };

        let lines: Vec<&str> = raw.lines().collect();
        let total = lines.len();

        if params.0.offset.is_none() && params.0.limit.is_none() {
            // Backward-compatible: return full content with total_lines header
            let header = format!("({} lines total)\n", total);
            return Ok(CallToolResult::success(vec![Content::text(format!("{}{}", header, raw))]));
        }

        let offset = params.0.offset.unwrap_or(1).max(1);
        let limit  = params.0.limit.unwrap_or(300);
        let start  = (offset - 1).min(total);
        let end    = (start + limit).min(total);

        let mut out = format!("Lines {}–{} of {} total\n", offset, offset + (end - start).saturating_sub(1), total);
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:>6}: {}\n", start + i + 1, line));
        }
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// Write S-expression content to a KiCad file (create or overwrite).
    /// After writing a .kicad_pcb, call render_pcb or run_drc to check the result.
    #[tool(description = "Write KiCad S-expression content to a file (.kicad_pcb or .kicad_sch) — create or update a design")]
    async fn write_kicad_file(
        &self,
        params: Parameters<WritePcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&params.0.path);
        let _guard = self.lock_file(&path).await;
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent).await {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to create directory {}: {e}", parent.display()
                ))]));
            }
        }
        match fs::write(&path, &params.0.content).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Written to {}.\nNext steps: call render_pcb to preview, run_drc to check for violations.",
                path.display()
            ))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", path.display()
            ))])),
        }
    }

    // ---- KiCad CLI — Render ------------------------------------------------

    /// Render a PCB to a photorealistic 3D image using KiCad's built-in raytracer.
    /// When no side is specified, renders both top and bottom.
    #[tool(description = "Render a .kicad_pcb file to 3D preview PNG(s) using KiCad's raytracer. Omit 'side' to get both top and bottom views. Specify side for a single view: top, bottom, front, back, left, right.")]
    async fn render_pcb(
        &self,
        params: Parameters<RenderPcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let width = p.width.unwrap_or(1200).to_string();
        let height = p.height.unwrap_or(800).to_string();
        let quality = p.quality.as_deref().unwrap_or("high");
        let zoom = p.zoom.unwrap_or(1.5).to_string();

        // If a specific side was requested, render only that. Otherwise render top + bottom.
        let sides: Vec<&str> = match p.side.as_deref() {
            Some(s) => vec![s],
            None => vec!["top", "bottom"],
        };

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let mut contents: Vec<Content> = Vec::new();
        for side in &sides {
            let out_path = std::env::temp_dir().join(format!("kicad_render_{ts}_{side}.png"));
            let (_, stderr, code) = run_kicad_cli(&[
                "pcb", "render",
                "--output", out_path.to_str().unwrap_or("/tmp/render.png"),
                "--width", &width,
                "--height", &height,
                "--side", side,
                "--quality", quality,
                "--background", "opaque",
                "--zoom", &zoom,
                &p.path,
            ]).await?;

            if code != 0 {
                contents.push(Content::text(format!("{side} render failed: {stderr}")));
                continue;
            }

            match fs::read(&out_path).await {
                Ok(bytes) => {
                    let _ = fs::remove_file(&out_path).await;
                    contents.push(Content::text(format!("{side} view:")));
                    contents.push(Content::image(BASE64_STANDARD.encode(&bytes), "image/png"));
                }
                Err(e) => {
                    contents.push(Content::text(format!("{side} render failed to read: {e}")));
                }
            }
        }

        if contents.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text("All renders failed.".to_string())]));
        }
        Ok(CallToolResult::success(contents))
    }

    // ---- KiCad CLI — DRC ---------------------------------------------------

    /// Run KiCad's Design Rules Check on a PCB file.
    /// Returns a structured JSON report of all violations, clearance errors, and unconnected nets.
    #[tool(description = "Run DRC (Design Rules Check) on a .kicad_pcb file and return a JSON report of violations")]
    async fn run_drc(
        &self,
        params: Parameters<DrcParams>,
    ) -> Result<CallToolResult, McpError> {
        let out_path = std::env::temp_dir().join(format!(
            "kicad_drc_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "pcb", "drc",
            "--output", out_path.to_str().unwrap_or("/tmp/drc.json"),
            "--format", "json",
            "--severity-all",
            "--units", "mm",
            &params.0.path,
        ]).await?;

        match fs::read_to_string(&out_path).await {
            Ok(report) => {
                let _ = fs::remove_file(&out_path).await;
                let mut contents = vec![Content::text(report)];
                // Append a board render so violations are visually obvious
                contents.extend(self.render_board(&params.0.path).await);
                Ok(CallToolResult::success(contents))
            }
            Err(_) => {
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "DRC failed (exit {code}):\n{stderr}"
                ))]))
            }
        }
    }

    // ---- KiCad CLI — Schematic tools ---------------------------------------

    /// Export the schematic netlist. Shows every component, its reference, value,
    /// footprint, and all net connections — essential for understanding the design
    /// before modifying the PCB.
    #[tool(description = "Export the schematic netlist from a .kicad_sch file — shows all components and net connections")]
    async fn export_netlist(
        &self,
        params: Parameters<SchematicParams>,
    ) -> Result<CallToolResult, McpError> {
        let out_path = std::env::temp_dir().join(format!(
            "kicad_netlist_{}.net",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "sch", "export", "netlist",
            "--output", out_path.to_str().unwrap_or("/tmp/netlist.net"),
            "--format", "kicadsexpr",
            &params.0.path,
        ]).await?;

        match fs::read_to_string(&out_path).await {
            Ok(content) => {
                let _ = fs::remove_file(&out_path).await;
                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(_) => Ok(CallToolResult::error(vec![Content::text(format!(
                "netlist export failed (exit {code}):\n{stderr}"
            ))])),
        }
    }

    /// Export the Bill of Materials as CSV.
    /// Gives a quick inventory of all components: reference, value, footprint, quantity.
    #[tool(description = "Export a Bill of Materials (BOM) CSV from a .kicad_sch schematic file")]
    async fn export_bom(
        &self,
        params: Parameters<SchematicParams>,
    ) -> Result<CallToolResult, McpError> {
        let out_path = std::env::temp_dir().join(format!(
            "kicad_bom_{}.csv",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "sch", "export", "bom",
            "--output", out_path.to_str().unwrap_or("/tmp/bom.csv"),
            &params.0.path,
        ]).await?;

        match fs::read_to_string(&out_path).await {
            Ok(content) => {
                let _ = fs::remove_file(&out_path).await;
                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(_) => Ok(CallToolResult::error(vec![Content::text(format!(
                "BOM export failed (exit {code}):\n{stderr}"
            ))])),
        }
    }

    // ---- Footprint library tools -------------------------------------------

    /// List all available footprint libraries (.pretty directories).
    /// Shows both system-installed libraries and any project-local ones.
    #[tool(description = "List all available KiCad footprint libraries (.pretty directories) on this system")]
    async fn list_footprint_libraries(
        &self,
        _params: Parameters<NoParams>,
    ) -> Result<CallToolResult, McpError> {
        let dirs = collect_all_pretty_dirs(&self.fp_lib_dirs, None).await;

        if dirs.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No footprint libraries found.\n\
                 Install them with: sudo pacman -S kicad-library\n\
                 Or download via KiCad → PCM (Package and Content Manager)."
                    .to_string(),
            )]));
        }

        let mut lines = vec![format!("Found {} footprint libraries:\n", dirs.len())];
        for dir in &dirs {
            // Show just the library name (stem), not the full path
            let name = dir
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            lines.push(format!("  {name}  ({})", dir.display()));
        }
        Ok(CallToolResult::success(vec![Content::text(lines.join("\n"))]))
    }

    /// List all footprints available in a specific library.
    /// Use search_footprint to find which library contains a component.
    #[tool(description = "List all footprints in a KiCad footprint library (e.g. 'Connector_PinHeader_2.54mm')")]
    async fn list_footprints_in_library(
        &self,
        params: Parameters<LibraryParams>,
    ) -> Result<CallToolResult, McpError> {
        let lib_path = match self.resolve_library(&params.0.library) {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Library '{}' not found. Use list_footprint_libraries to see available libraries.",
                    params.0.library
                ))]));
            }
        };

        let mut entries = Vec::new();
        if let Ok(mut rd) = fs::read_dir(&lib_path).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let p = entry.path();
                if p.extension().map(|e| e == "kicad_mod").unwrap_or(false) {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        entries.push(stem.to_string());
                    }
                }
            }
        }

        entries.sort();
        if entries.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No footprints found in library '{}'", params.0.library
            ))]));
        }

        let text = format!(
            "Library: {} ({} footprints)\n\n{}",
            params.0.library,
            entries.len(),
            entries.join("\n")
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Get the full S-expression content of a footprint from a library.
    /// The returned text can be embedded directly inside a (footprint ...) node in a .kicad_pcb file.
    #[tool(description = "Get the full .kicad_mod S-expression for a footprint — use this to embed a footprint into a PCB file")]
    async fn get_footprint(
        &self,
        params: Parameters<GetFootprintParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let lib_path = match self.resolve_library(&p.library) {
            Some(path) => path,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Library '{}' not found. Use list_footprint_libraries to see what's available.",
                    p.library
                ))]));
            }
        };

        let fp_path = lib_path.join(format!("{}.kicad_mod", p.footprint));
        match fs::read_to_string(&fp_path).await {
            Ok(content) => Ok(CallToolResult::success(vec![Content::text(content)])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Footprint '{}/{}' not found: {e}\n\
                 Use list_footprints_in_library to see available footprints.",
                p.library, p.footprint
            ))])),
        }
    }

    /// Search for footprints by name across all installed libraries.
    /// Returns matching library/footprint pairs — use get_footprint to retrieve one.
    #[tool(description = "Search for footprints by name across all KiCad libraries (case-insensitive substring match)")]
    async fn search_footprint(
        &self,
        params: Parameters<SearchFootprintParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.0.query.to_lowercase();
        let max = params.0.max_results.unwrap_or(30);

        let project_hint = params.0.project_path.as_deref().map(Path::new);
        let all_libs = collect_all_pretty_dirs(&self.fp_lib_dirs, project_hint).await;

        if all_libs.is_empty() {
            let searched: Vec<String> = {
                let mut dirs = Vec::new();
                if let Ok(home) = std::env::var("HOME") {
                    dirs.push(format!("  {}/.local/share/kicad/9.0/footprints", home));
                    dirs.push(format!("  {}/.local/share/kicad/8.0/footprints", home));
                }
                dirs.push("  /usr/share/kicad/footprints".into());
                dirs.push("  /usr/local/share/kicad/footprints".into());
                if let Some(p) = project_hint {
                    dirs.push(format!("  {} (project-local)", p.display()));
                }
                dirs
            };
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No footprint libraries found. Searched:\n{}\n\n\
                 Install system libraries: sudo pacman -S kicad-library\n\
                 Or pass project_path to discover project-local .pretty directories.",
                searched.join("\n")
            ))]));
        }

        let mut matches = Vec::new();

        'outer: for lib_dir in &all_libs {
            let lib_name = lib_dir
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();

            if let Ok(mut rd) = fs::read_dir(lib_dir).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    if matches.len() >= max {
                        break 'outer;
                    }
                    let p = entry.path();
                    if p.extension().map(|e| e == "kicad_mod").unwrap_or(false) {
                        let fp_name = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string();
                        let combined = format!("{}:{}", lib_name, fp_name).to_lowercase();
                        if combined.contains(&query) {
                            matches.push(format!("{lib_name}:{fp_name}"));
                        }
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No footprints matching '{}' found across {} libraries.",
                params.0.query,
                all_libs.len()
            ))]));
        }

        matches.sort();
        let text = format!(
            "Found {} match(es) for '{}'{}:\n\n{}",
            matches.len(),
            params.0.query,
            if matches.len() >= max { " (truncated)" } else { "" },
            matches.join("\n")
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    // ---- Project scanner ---------------------------------------------------

    /// Scan a directory for KiCad project files and return a full project summary:
    /// all .kicad_pcb, .kicad_sch, and .kicad_pro files found, the BOM if a schematic
    /// is present, and a rendered top-view image of each PCB.
    /// Use this as the first call when starting work on a project.
    #[tool(description = "START HERE for any KiCad task. Scans a project folder and returns: all .kicad_pcb/.kicad_sch/.kicad_pro files found, a BOM, and a rendered top-view image of each PCB. Always call this first before grep, read, or any edits.")]
    async fn scan_project(
        &self,
        params: Parameters<ScanProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = PathBuf::from(&params.0.path);

        // Collect all KiCad files recursively (up to 3 levels deep)
        let mut pcb_files: Vec<PathBuf> = Vec::new();
        let mut sch_files: Vec<PathBuf> = Vec::new();
        let mut pro_files: Vec<PathBuf> = Vec::new();

        collect_kicad_files(&root, 0, 3, &mut pcb_files, &mut sch_files, &mut pro_files).await;

        if pcb_files.is_empty() && sch_files.is_empty() && pro_files.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "No KiCad files found in '{}'. Make sure the path is correct.",
                root.display()
            ))]));
        }

        let mut contents: Vec<Content> = Vec::new();
        let mut summary = format!("KiCad project at: {}\n\n", root.display());

        // List all found files
        if !pro_files.is_empty() {
            summary.push_str("Project files:\n");
            for f in &pro_files { summary.push_str(&format!("  {}\n", f.display())); }
            summary.push('\n');
        }
        if !pcb_files.is_empty() {
            summary.push_str("PCB files:\n");
            for f in &pcb_files { summary.push_str(&format!("  {}\n", f.display())); }
            summary.push('\n');
        }
        if !sch_files.is_empty() {
            summary.push_str("Schematic files:\n");
            for f in &sch_files { summary.push_str(&format!("  {}\n", f.display())); }
            summary.push('\n');
        }

        contents.push(Content::text(summary));

        // Export BOM from the first schematic (top-level preferred)
        if let Some(sch) = sch_files.first() {
            let out_path = std::env::temp_dir().join(format!(
                "kicad_scan_bom_{}.csv",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ));
            let _ = run_kicad_cli(&[
                "sch", "export", "bom",
                "--output", out_path.to_str().unwrap_or("/tmp/bom.csv"),
                sch.to_str().unwrap_or(""),
            ]).await;
            if let Ok(bom) = fs::read_to_string(&out_path).await {
                let _ = fs::remove_file(&out_path).await;
                contents.push(Content::text(format!("Bill of Materials:\n{bom}")));
            }
        }

        // Render each PCB (top view)
        for pcb in &pcb_files {
            contents.push(Content::text(format!("PCB render — {}:", pcb.display())));
            let render_imgs = self.render_board(pcb.to_str().unwrap_or("")).await;
            if !render_imgs.is_empty() {
                contents.extend(render_imgs);
            } else {
                // render_board returned nothing (e.g. kicad-cli error) — skip silently
            }
        }

        Ok(CallToolResult::success(contents))
    }

    // ---- KiCad CLI — SVG export --------------------------------------------

    /// Export one or more PCB layers as SVG — useful for inspecting copper routing,
    /// silkscreen, or fab layers during editing. Returns the SVG text content.
    #[tool(description = "Export PCB layers as SVG (e.g. 'F.Cu,B.Cu,Edge.Cuts') for detailed layer inspection")]
    async fn export_layer_svg(
        &self,
        params: Parameters<ExportSvgParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let out_path = std::env::temp_dir().join(format!(
            "kicad_svg_{}.svg",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "pcb", "export", "svg",
            "--output", out_path.to_str().unwrap_or("/tmp/out.svg"),
            "--layers", &p.layers,
            "--fit-page-to-board",
            "--mode-single",
            &p.path,
        ]).await?;

        let svg_content = match fs::read_to_string(&out_path).await {
            Ok(svg) => svg,
            Err(_) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "SVG export failed (exit {code}):\n{stderr}"
            ))])),
        };

        let mut contents: Vec<Content> = Vec::new();

        // Convert SVG → PNG via rsvg-convert for visual inspection
        let png_path = out_path.with_extension("png");
        let conv = Command::new("rsvg-convert")
            .args([
                "-w", "2400",
                "-o", png_path.to_str().unwrap_or("/tmp/out.png"),
                out_path.to_str().unwrap_or("/tmp/out.svg"),
            ])
            .output()
            .await;

        let _ = fs::remove_file(&out_path).await;

        if conv.map(|o| o.status.success()).unwrap_or(false) {
            if let Ok(bytes) = fs::read(&png_path).await {
                let _ = fs::remove_file(&png_path).await;
                contents.push(Content::text(format!("Layer SVG rendered — layers: {}", p.layers)));
                contents.push(Content::image(BASE64_STANDARD.encode(&bytes), "image/png"));
                // Also include raw SVG text for precise coordinate inspection
                contents.push(Content::text(svg_content));
                return Ok(CallToolResult::success(contents));
            }
        }

        // rsvg-convert not available — return SVG text only
        contents.push(Content::text(format!(
            "Layer SVG exported (rsvg-convert not available for PNG preview):\n{svg_content}"
        )));
        Ok(CallToolResult::success(contents))
    }

    // ---- Component-level PCB editing tools --------------------------------

    /// Get a single component's footprint block from a PCB file, plus a metadata summary.
    /// Much lighter than read_kicad_file — returns only the one component, not the full file.
    #[tool(description = "Get a single component's position, footprint, value, and S-expression block from a .kicad_pcb file by reference (e.g. 'U1', 'C3'). Use this instead of grep or read_kicad_file when you need to inspect one component — it is much faster and returns only what you need.")]
    async fn get_component(
        &self,
        params: Parameters<GetComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let blocks = pcb_edit::find_footprint_blocks(&content);
        let range = match blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => {
                let known: Vec<&str> = blocks.keys().map(String::as_str).collect();
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Reference '{}' not found in {}.\nAvailable: {}",
                    p.reference, p.path, known.join(", ")
                ))]));
            }
        };

        let block = &content[range];
        let fp_name = pcb_edit::extract_fp_name(block).unwrap_or_default();
        let value   = pcb_edit::fp_text_value(block, "value").unwrap_or_default();
        let (x, y, rot) = pcb_edit::extract_at(block).unwrap_or_default();

        let summary = format!(
            "{ref} | {value} | {fp_name} | position ({x}, {y}, rot={rot}°)",
            ref = p.reference
        );

        Ok(CallToolResult::success(vec![
            Content::text(summary),
            Content::text(block.to_string()),
        ]))
    }

    /// Replace a component's footprint in a PCB file with a different one.
    /// Preserves the component's position, rotation, reference, value, and tstamp by default.
    /// After replacing, the board is re-rendered for immediate visual verification.
    #[tool(description = "Replace a component's footprint in a .kicad_pcb file. Loads the new .kicad_mod from a library, preserves the existing position, rotation, reference, value, and tstamp, writes the file, and returns an updated render. Use this instead of writing Python scripts or manually editing footprint blocks.")]
    async fn replace_footprint(
        &self,
        params: Parameters<ReplaceFootprintParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let blocks = pcb_edit::find_footprint_blocks(&content);
        let range = match blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Reference '{}' not found in {}.", p.reference, p.path
            ))])),
        };
        let old_block = &content[range.clone()];

        let (x, y, rot) = if p.keep_position.unwrap_or(true) {
            pcb_edit::extract_at(old_block).unwrap_or((0.0, 0.0, 0.0))
        } else {
            (0.0, 0.0, 0.0)
        };
        let _tstamp = pcb_edit::extract_tstamp(old_block)
            .unwrap_or_else(pcb_edit::new_tstamp);
        let _value = pcb_edit::fp_text_value(old_block, "value")
            .unwrap_or_else(|| p.reference.clone());

        let lib_path = match self.resolve_library(&p.library) {
            Some(lp) => lp,
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Library '{}' not found. Use list_footprint_libraries to see what's available.",
                p.library
            ))])),
        };

        let fp_path = lib_path.join(format!("{}.kicad_mod", p.footprint));
        if !fp_path.exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Footprint '{}/{}' not found.",
                p.library, p.footprint
            ))]));
        }

        let lib_path_str = lib_path.to_str().unwrap_or("").to_string();
        let old_fp_name = pcb_edit::extract_fp_name(old_block).unwrap_or_default();

        // Use pcbnew Python API to replace the footprint. This is more reliable than
        // text injection: it handles all KiCad format versions, sets the reference
        // correctly (no REF** placeholder), and returns the new pad positions.
        let script = format!(r#"
import pcbnew, json, sys
b = pcbnew.LoadBoard({pcb:?})
old = b.FindFootprintByReference({reference:?})
if old is None:
    print(json.dumps({{"error": "reference not found"}})); sys.exit(1)
keep_pos = {keep_pos}
pos = old.GetPosition() if keep_pos else pcbnew.VECTOR2I(0, 0)
rot = old.GetOrientation() if keep_pos else pcbnew.EDA_ANGLE(0, pcbnew.DEGREES_T)
val = old.GetValue()
# Capture old pad nets by pad number before the old footprint is removed, so the
# new footprint (freshly loaded from the library, which carries no net data at
# all) can have matching-numbered pads reconnected automatically — mirrors what
# KiCad's own "Change footprint" GUI action does.
old_nets = {{p.GetNumber(): p.GetNetname() for p in old.Pads() if p.GetNetname()}}
new_fp = pcbnew.FootprintLoad({lib:?}, {fp:?})
if new_fp is None:
    print(json.dumps({{"error": "footprint not found in library"}})); sys.exit(1)
new_fp.SetReference({reference:?})
new_fp.SetValue(val)
new_fp.SetPosition(pos)
new_fp.SetOrientation(rot)
b.Remove(old)
b.Add(new_fp)
carried = []
uncarried_old = []
for pad in new_fp.Pads():
    net_name = old_nets.get(pad.GetNumber())
    if net_name:
        net_info = b.FindNet(net_name)
        if net_info is not None:
            pad.SetNet(net_info)
            carried.append(pad.GetNumber())
for num, net_name in old_nets.items():
    if num not in carried:
        uncarried_old.append({{"number": num, "net": net_name}})
b.Save({pcb:?})
pads = [{{"number": p.GetNumber(),
          "x": round(pcbnew.ToMM(p.GetPosition().x), 6),
          "y": round(pcbnew.ToMM(p.GetPosition().y), 6),
          "layer": p.GetLayerName(),
          "net": p.GetNetname()}} for p in new_fp.Pads()]
print(json.dumps({{"ok": True, "pads": pads, "old_pad_count": len(old_nets), "carried_count": len(carried), "uncarried_old": uncarried_old}}))
"#,
            pcb = p.path,
            reference = p.reference,
            keep_pos = if p.keep_position.unwrap_or(true) { "True" } else { "False" },
            lib = lib_path_str,
            fp = p.footprint,
        );

        let py_out = Command::new("python3")
            .args(["-c", &script])
            .env("DISPLAY", "")
            .output().await
            .map_err(|e| McpError::internal_error(format!("python3 failed: {e}"), None))?;

        if !py_out.status.success() {
            let err = String::from_utf8_lossy(&py_out.stderr);
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "replace_footprint failed:\n{err}"
            ))]));
        }

        let py_json: serde_json::Value = serde_json::from_str(
            &String::from_utf8_lossy(&py_out.stdout)
        ).unwrap_or(serde_json::Value::Null);

        if let Some(err) = py_json.get("error") {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "replace_footprint error: {err}"
            ))]));
        }

        // Format pad position + net table for the response
        let pad_table = if let Some(pads) = py_json.get("pads").and_then(|p| p.as_array()) {
            let mut lines = vec!["Pad positions:".to_string()];
            for pad in pads {
                let num = pad["number"].as_str().unwrap_or("?");
                let px  = pad["x"].as_f64().unwrap_or(0.0);
                let py  = pad["y"].as_f64().unwrap_or(0.0);
                let lay = pad["layer"].as_str().unwrap_or("?");
                let net = pad["net"].as_str().unwrap_or("");
                if net.is_empty() {
                    lines.push(format!("  pad {num}: ({px:.4}, {py:.4}) {lay}  [no net]"));
                } else {
                    lines.push(format!("  pad {num}: ({px:.4}, {py:.4}) {lay}  net={net}"));
                }
            }
            lines.join("\n")
        } else {
            String::new()
        };

        // Net carryover summary — the old footprint's pads are matched to the new
        // footprint's pads by pad NUMBER; anything left unmatched needs manual
        // net reassignment (e.g. old/new footprints have different pad counts).
        let old_pad_count = py_json.get("old_pad_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let carried_count = py_json.get("carried_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let net_summary = if old_pad_count == 0 {
            "No netted pads on the old footprint — nothing to carry over.".to_string()
        } else {
            let mut s = format!("Carried over nets for {carried_count}/{old_pad_count} old netted pad(s).");
            if let Some(uncarried) = py_json.get("uncarried_old").and_then(|v| v.as_array()) {
                if !uncarried.is_empty() {
                    let list: Vec<String> = uncarried.iter().map(|u| {
                        let num = u["number"].as_str().unwrap_or("?");
                        let net = u["net"].as_str().unwrap_or("?");
                        format!("pad {num} (was {net})")
                    }).collect();
                    s.push_str(&format!(
                        "\n⚠️  No matching pad number on the new footprint for: {} — reassign manually.",
                        list.join(", ")
                    ));
                }
            }
            s
        };

        let summary = format!(
            "Replaced {} footprint: {} → {}:{}\nPosition kept: ({}, {}, rot={}°)\n{}\n\n{}",
            p.reference, old_fp_name, p.library, p.footprint, x, y, rot, pad_table, net_summary
        );

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Delete one or more components from a PCB file by reference designator.
    /// Useful for removing obsolete parts (e.g. discrete caps replaced by a module).
    /// Returns an updated render after removal.
    #[tool(description = "Remove one or more components from a .kicad_pcb file by reference designator — pass a list like [\"C1\",\"C3\",\"C4\",\"C5\"] to delete multiple at once. Writes the file and returns an updated render. Use this instead of Python scripts or manual line deletion.")]
    async fn delete_component(
        &self,
        params: Parameters<DeleteComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let blocks = pcb_edit::find_footprint_blocks(&content);
        let mut found   = Vec::new();
        let mut missing = Vec::new();
        for r in &p.refs {
            if blocks.contains_key(r.as_str()) {
                found.push(r.as_str());
            } else {
                missing.push(r.as_str());
            }
        }

        if found.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "None of the requested refs found: {}",
                p.refs.join(", ")
            ))]));
        }

        let new_content = pcb_edit::remove_footprints(&content, &found);

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", p.path
            ))]));
        }

        let mut summary = format!("Removed {} component(s): {}", found.len(), found.join(", "));
        if !missing.is_empty() {
            summary.push_str(&format!("\nNot found (skipped): {}", missing.join(", ")));
        }

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Create a new footprint (.kicad_mod) from a list of pad definitions.
    /// Writes to the specified .pretty library directory.
    /// Use this to define custom connectors, modules, or breakout boards.
    #[tool(description = "Create a new .kicad_mod footprint file in a .pretty library directory from a list of pad definitions")]
    async fn create_footprint(
        &self,
        params: Parameters<CreateFootprintParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let lib_path = PathBuf::from(&p.library_path);

        if !lib_path.is_dir() {
            if let Err(e) = fs::create_dir_all(&lib_path).await {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to create library directory {}: {e}", p.library_path
                ))]));
            }
        }

        let margin = p.courtyard_margin.unwrap_or(0.25);
        let descr = p.description.as_deref().unwrap_or("");
        let tags  = p.tags.as_deref().unwrap_or("");

        // Compute courtyard bounds from pad extents
        let (min_x, min_y, max_x, max_y) = p.pads.iter().fold(
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
            |(lx, ly, hx, hy), pad| {
                let hx2 = pad.size_x / 2.0;
                let hy2 = pad.size_y / 2.0;
                (
                    lx.min(pad.x - hx2),
                    ly.min(pad.y - hy2),
                    hx.max(pad.x + hx2),
                    hy.max(pad.y + hy2),
                )
            },
        );
        let cyd = (
            min_x - margin,
            min_y - margin,
            max_x + margin,
            max_y + margin,
        );

        // Build pad S-expressions
        let pads_str: String = p.pads.iter().map(|pad| {
            let drill_str = if let Some(d) = pad.drill {
                format!(" (drill {})", d)
            } else {
                String::new()
            };
            let layers = if pad.pad_type == "smd" {
                r#"(layers "F.Cu" "F.Paste" "F.Mask")"#
            } else {
                r#"(layers "*.Cu" "*.Mask")"#
            };
            let shape = if pad.shape == "rect" { "rect" }
                        else if pad.shape == "oval" { "oval" }
                        else { "circle" };
            let pad_shape = if pad.number == "1" { "rect" } else { shape };
            format!(
                "  (pad \"{}\" {} {} (at {} {}) (size {} {}){} {})\n",
                pad.number, pad.pad_type, pad_shape,
                pad.x, pad.y, pad.size_x, pad.size_y,
                drill_str, layers
            )
        }).collect();

        let content = format!(
            "(footprint \"{}\" (version 20211014) (generator pcbnew)\n\
               (layer \"F.Cu\")\n\
               (descr \"{}\")\n\
               (tags \"{}\")\n\
               (attr {})\n\
               (fp_text reference \"REF**\" (at 0 {}) (layer \"F.SilkS\")\n\
               (effects (font (size 1 1) (thickness 0.15)))\n\
             )\n\
               (fp_text value \"{}\" (at 0 {}) (layer \"F.Fab\")\n\
               (effects (font (size 1 1) (thickness 0.15)))\n\
             )\n\
             {}\
               (fp_rect (start {} {}) (end {} {}) (layer \"F.CrtYd\") (width 0.05) (fill none))\n\
             )",
            p.name,
            descr,
            tags,
            if p.pads.iter().any(|pd| pd.pad_type == "smd") { "smd" } else { "through_hole" },
            cyd.1 - margin - 1.5,  // ref above courtyard
            p.name,
            cyd.3 + margin + 1.5,  // value below courtyard
            pads_str,
            cyd.0, cyd.1, cyd.2, cyd.3
        );

        let out_path = lib_path.join(format!("{}.kicad_mod", p.name));
        if let Err(e) = fs::write(&out_path, &content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write footprint: {e}"
            ))]));
        }

        Ok(CallToolResult::success(vec![
            Content::text(format!(
                "Created footprint '{}' at {}\n{} pad(s) defined.",
                p.name, out_path.display(), p.pads.len()
            )),
            Content::text(content),
        ]))
    }

    // ---- File search and patch tools ----------------------------------------

    /// Search for a string inside a KiCad file and return matching lines with context.
    /// Useful for locating a specific net, component, or attribute before editing.
    #[tool(description = "Search for a string inside any KiCad file (.kicad_pcb or .kicad_sch) and return matching lines with surrounding context and line numbers. Use this instead of shell grep — it handles large files safely and is the right first step before patch_kicad_file.")]
    async fn grep_kicad_file(
        &self,
        params: Parameters<GrepKicadParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let raw = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let context  = p.context_lines.unwrap_or(3);
        let max_hits = p.max_matches.unwrap_or(20);
        let lines: Vec<&str> = raw.lines().collect();
        let total = lines.len();

        // Collect matching line indices (0-based)
        let hit_indices: Vec<usize> = lines.iter().enumerate()
            .filter(|(_, l)| l.contains(&p.query))
            .map(|(i, _)| i)
            .take(max_hits)
            .collect();

        if hit_indices.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No matches for '{}' in {}", p.query, p.path
            ))]));
        }

        let truncated = hit_indices.len() >= max_hits;

        // Build output, deduplicating overlapping context windows
        let mut out = String::new();
        let mut printed_up_to: usize = 0; // last printed line (0-based), exclusive
        for (match_num, &hit) in hit_indices.iter().enumerate() {
            let win_start = hit.saturating_sub(context);
            let win_end   = (hit + context + 1).min(total);

            out.push_str(&format!("\nMatch {} (line {}):\n", match_num + 1, hit + 1));
            let print_from = win_start.max(printed_up_to);
            for i in print_from..win_end {
                if i == hit {
                    out.push_str(&format!("> {:>4}: {}\n", i + 1, lines[i]));
                } else {
                    out.push_str(&format!("  {:>4}: {}\n", i + 1, lines[i]));
                }
            }
            printed_up_to = win_end;
        }

        if truncated {
            out.push_str(&format!("\n(showing first {} matches — use a more specific query to narrow results)", max_hits));
        }

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// Read a specific line range from a KiCad file with line-number annotations.
    /// Use this for paginated reading of large files.
    #[tool(description = "Read a specific line range from a KiCad file with line numbers — use offset/limit to paginate through large files")]
    async fn read_kicad_section(
        &self,
        params: Parameters<ReadSectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let raw = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let lines: Vec<&str> = raw.lines().collect();
        let total = lines.len();
        let offset = p.offset.unwrap_or(1).max(1);
        let limit  = p.limit.unwrap_or(300);
        let start  = (offset - 1).min(total);
        let end    = (start + limit).min(total);

        let mut out = format!("Lines {}–{} of {} total\n", offset, offset + (end - start).saturating_sub(1), total);
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:>6}: {}\n", start + i + 1, line));
        }
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// Perform an exact string replacement in a KiCad file.
    /// Always use grep_kicad_file first to verify the exact text, then patch.
    #[tool(description = "Perform an exact string replacement in any KiCad file (.kicad_pcb or .kicad_sch). Use grep_kicad_file first to confirm the exact text including whitespace, then call this. For schematic files, automatically renders a PNG preview after the edit. Use this instead of Python scripts or sed.")]
    async fn patch_kicad_file(
        &self,
        params: Parameters<PatchKicadParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let _guard = self.lock_file(Path::new(&p.path)).await;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let count = content.matches(&p.old_string).count();
        if count == 0 {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "old_string not found in {}\n\
                 Use grep_kicad_file to locate the exact text (including whitespace).",
                p.path
            ))]));
        }

        let replace_all = p.replace_all.unwrap_or(false);
        if count > 1 && !replace_all {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "old_string matches {} times in {} — provide more context to make it unique, or set replace_all=true",
                count, p.path
            ))]));
        }

        let new_content = if replace_all {
            content.replace(&p.old_string, &p.new_string)
        } else {
            content.replacen(&p.old_string, &p.new_string, 1)
        };

        if let Err(e) = fs::write(&p.path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", p.path
            ))]));
        }

        // Build diff context: use the ACTUAL match byte-offset (known from the
        // `count`/uniqueness check above via old_string) rather than re-searching
        // the whole file for new_string's first line — that re-search could latch
        // onto an earlier, unrelated occurrence of similar/generic boilerplate
        // elsewhere in a large file, showing the wrong location entirely even
        // though the edit itself (via replacen/replace, above) landed correctly.
        // The region before the first match is byte-identical between `content`
        // and `new_content` (nothing before it moves), so counting newlines in
        // the ORIGINAL content up to that offset gives the correct line number
        // in `new_content` too. For replace_all, this anchors on the first of
        // potentially several replacements — sufficient to restore trust in the
        // preview without listing every location.
        let replacements = if replace_all { count } else { 1 };
        let diff_ctx: String = {
            let match_offset = content.find(&p.old_string).unwrap_or(0);
            let hit_line = content[..match_offset].matches('\n').count();
            let new_lines: Vec<&str> = new_content.lines().collect();
            let win_start = hit_line.saturating_sub(3);
            let win_end   = (hit_line + 4).min(new_lines.len());
            new_lines[win_start..win_end].iter().enumerate()
                .map(|(i, l)| format!("  {:>4}: {}", win_start + i + 1, l))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut result_contents = vec![Content::text(format!(
            "Patched {} — {} replacement(s) made.\n\nContext after edit:\n{}",
            p.path, replacements, diff_ctx
        ))];

        // Auto-render preview for schematic files unless explicitly disabled
        let want_preview = p.render_preview.unwrap_or(true);
        if want_preview && p.path.ends_with(".kicad_sch") {
            if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
                result_contents.push(img);
            }
        } else if want_preview && p.path.ends_with(".kicad_pcb") {
            result_contents.extend(self.render_board(&p.path).await);
        }

        Ok(CallToolResult::success(result_contents))
    }

    // ---- Schematic rendering ------------------------------------------------

    /// Render a KiCad schematic to a PNG image for visual inspection.
    /// Uses kicad-cli to export SVG then converts to PNG — no display required.
    #[tool(description = "Render a .kicad_sch schematic to a PNG preview image — call after editing to visually verify the schematic looks correct")]
    async fn render_schematic(
        &self,
        params: Parameters<RenderSchematicParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let width = p.width.unwrap_or(2400);
        let bw = p.black_and_white.unwrap_or(false);

        match self.render_schematic_png(&p.path, p.theme.as_deref(), bw, width).await {
            Some(img) => Ok(CallToolResult::success(vec![
                Content::text(format!("Schematic render — {}", p.path)),
                img,
            ])),
            None => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to render schematic {}.\n\
                 Ensure kicad-cli and rsvg-convert are installed and the file is valid.",
                p.path
            ))])),
        }
    }

    // ---- PCB component placement -------------------------------------------

    /// Move a component to a new absolute position and/or rotation.
    /// Use after replace_footprint when the new footprint has different dimensions
    /// and the component needs repositioning. Returns an updated render.
    #[tool(description = "Move a PCB component to a new absolute position and rotation. Use after replace_footprint when the new footprint needs repositioning. Returns an updated render.")]
    async fn move_component(
        &self,
        params: Parameters<MoveComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let blocks = pcb_edit::find_footprint_blocks(&content);
        let range = match blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Reference '{}' not found in {}.", p.reference, p.path
            ))])),
        };

        let old_block = &content[range.clone()];
        let (old_x, old_y, old_rot) = pcb_edit::extract_at(old_block).unwrap_or((0.0, 0.0, 0.0));
        let new_x = p.x.unwrap_or_else(|| old_x + p.dx.unwrap_or(0.0));
        let new_y = p.y.unwrap_or_else(|| old_y + p.dy.unwrap_or(0.0));
        let rot = p.rotation.unwrap_or(old_rot);

        let new_block = pcb_edit::replace_at(old_block, new_x, new_y, rot);
        let new_content = format!(
            "{}{}{}",
            &content[..range.start],
            new_block,
            &content[range.end..]
        );

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", p.path
            ))]));
        }

        let summary = format!(
            "Moved {} to ({}, {}, rot={}°)",
            p.reference, new_x, new_y, rot
        );

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Place a new component (footprint) into a PCB file at a given position.
    /// The footprint is loaded from the library, upgraded to the current format,
    /// and appended to the board. Returns an updated render.
    #[tool(description = "Add a new component to a .kicad_pcb file — loads the footprint from a library and places it at the specified position. Returns an updated render.")]
    async fn add_component(
        &self,
        params: Parameters<AddComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        let lib_path = match self.resolve_library(&p.library) {
            Some(lp) => lp,
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Library '{}' not found.", p.library
            ))])),
        };

        let fp_path = lib_path.join(format!("{}.kicad_mod", p.footprint));
        if !fp_path.exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Footprint '{}/{}' not found.", p.library, p.footprint
            ))]));
        }

        let lib_nickname = lib_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&p.library)
            .to_string();

        let mod_content = upgrade_footprint_format(&fp_path, &p.footprint).await
            .unwrap_or_else(|_| std::fs::read_to_string(&fp_path).unwrap_or_default());

        if mod_content.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read footprint '{}/{}'.", p.library, p.footprint
            ))]));
        }

        let tstamp = pcb_edit::new_tstamp();
        let rot = p.rotation.unwrap_or(0.0);
        let new_block = pcb_edit::kicad_mod_to_pcb_footprint(
            &mod_content,
            &lib_nickname,
            &p.footprint,
            &p.reference,
            &p.value,
            p.x, p.y, rot,
            &tstamp,
        );

        // Insert before the closing ')' of the kicad_pcb root node
        let insert_pos = content.rfind("\n)").unwrap_or(content.len());
        let new_content = format!(
            "{}\n{}\n{}",
            &content[..insert_pos],
            new_block,
            &content[insert_pos..]
        );

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", p.path
            ))]));
        }

        let summary = format!(
            "Added {}:{} as {} ({}) at ({}, {}, rot={}°)",
            lib_nickname, p.footprint, p.reference, p.value, p.x, p.y, rot
        );

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    // ---- ERC / fabrication -------------------------------------------------

    /// Run KiCad's Electrical Rules Check on a schematic.
    /// Returns a structured JSON report of all ERC violations.
    #[tool(description = "Run ERC (Electrical Rules Check) on a .kicad_sch schematic and return a JSON report of violations — call after schematic edits to verify correctness")]
    async fn run_erc(
        &self,
        params: Parameters<ErcParams>,
    ) -> Result<CallToolResult, McpError> {
        let out_path = std::env::temp_dir().join(format!(
            "kicad_erc_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "sch", "erc",
            "--output", out_path.to_str().unwrap_or("/tmp/erc.json"),
            "--format", "json",
            "--severity-all",
            "--units", "mm",
            &params.0.path,
        ]).await?;

        match fs::read_to_string(&out_path).await {
            Ok(report) => {
                let _ = fs::remove_file(&out_path).await;
                Ok(CallToolResult::success(vec![Content::text(report)]))
            }
            Err(_) => Ok(CallToolResult::error(vec![Content::text(format!(
                "ERC failed (exit {code}):\n{stderr}"
            ))])),
        }
    }

    /// Export gerber, drill, and position files ready for board fabrication.
    /// Produces a directory (and optional zip) containing everything JLCPCB/PCBWay need.
    #[tool(description = "Export fabrication files (gerbers + drill + position) from a .kicad_pcb file into an output directory, ready for JLCPCB/PCBWay. Returns the list of generated files.")]
    async fn export_fabrication_files(
        &self,
        params: Parameters<ExportFabParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let pcb_path = PathBuf::from(&p.path);

        let out_dir = match p.output_dir {
            Some(ref d) => PathBuf::from(d),
            None => pcb_path.parent()
                .unwrap_or(Path::new("."))
                .join("fab"),
        };

        if let Err(e) = fs::create_dir_all(&out_dir).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to create output dir {}: {e}", out_dir.display()
            ))]));
        }

        let layers = p.layers.as_deref().unwrap_or(
            "F.Cu,B.Cu,F.Mask,B.Mask,F.SilkS,B.SilkS,F.Paste,B.Paste,Edge.Cuts"
        );

        let out_dir_str = out_dir.to_str().unwrap_or("/tmp");
        let pcb_str = p.path.as_str();

        // Gerbers
        let (_, gerber_err, gerber_code) = run_kicad_cli(&[
            "pcb", "export", "gerbers",
            "--output", out_dir_str,
            "--layers", layers,
            "--no-x2",
            pcb_str,
        ]).await?;

        // Drill files
        let (_, drill_err, drill_code) = run_kicad_cli(&[
            "pcb", "export", "drill",
            "--output", out_dir_str,
            "--format", "excellon",
            "--excellon-units", "mm",
            pcb_str,
        ]).await?;

        // Collect generated files
        let mut files: Vec<String> = Vec::new();
        if let Ok(mut rd) = fs::read_dir(&out_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                files.push(entry.path().display().to_string());
            }
        }
        files.sort();

        // Optionally zip
        let want_zip = p.zip.unwrap_or(true);
        let zip_path = if want_zip {
            let stem = pcb_path.file_stem().and_then(|s| s.to_str()).unwrap_or("board");
            let zp = out_dir.join(format!("{}_fab.zip", stem));
            let zp_str = zp.to_str().unwrap_or("/tmp/fab.zip");

            // Use zip CLI if available, otherwise skip
            let zip_result = Command::new("zip")
                .args(["-j", zp_str])
                .args(files.iter().map(String::as_str))
                .env("DISPLAY", "")
                .output()
                .await;

            if zip_result.map(|o| o.status.success()).unwrap_or(false) {
                Some(zp.display().to_string())
            } else {
                None
            }
        } else {
            None
        };

        let mut summary = String::new();
        if gerber_code != 0 { summary.push_str(&format!("Gerber warnings:\n{gerber_err}\n")); }
        if drill_code != 0  { summary.push_str(&format!("Drill warnings:\n{drill_err}\n")); }
        summary.push_str(&format!("\nGenerated {} file(s) in {}:\n", files.len(), out_dir.display()));
        for f in &files {
            summary.push_str(&format!("  {f}\n"));
        }
        if let Some(ref zp) = zip_path {
            summary.push_str(&format!("\nZip archive: {zp}"));
        }

        Ok(CallToolResult::success(vec![Content::text(summary)]))
    }

    // ---- Schematic wire / label editing ------------------------------------

    /// Add a wire segment to a .kicad_sch schematic file and return a preview image.
    #[tool(description = "Add a wire segment to a KiCad schematic (.kicad_sch) between two points (in mm) and return a preview image")]
    async fn add_wire(
        &self,
        params: Parameters<AddWireParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let mut content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        };

        let uuid = pcb_edit::new_tstamp();
        let wire_sexpr = format!(
            "\n  (wire (pts (xy {} {}) (xy {} {}))\n    (stroke (width 0) (type default))\n    (uuid \"{}\")\n  )",
            p.x1, p.y1, p.x2, p.y2, uuid
        );

        // Insert before the final closing paren of the file
        if let Some(pos) = content.rfind("\n)") {
            content.insert_str(pos, &wire_sexpr);
        } else {
            return Ok(CallToolResult::error(vec![Content::text(
                "File does not appear to be a valid KiCad schematic (no closing paren found)".to_string()
            )]));
        }

        if let Err(e) = fs::write(&path, &content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", path.display()
            ))]));
        }

        let mut items: Vec<Content> = vec![Content::text(format!(
            "Wire added: ({}, {}) → ({}, {})", p.x1, p.y1, p.x2, p.y2
        ))];
        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
            items.push(img);
        }
        Ok(CallToolResult::success(items))
    }

    /// Add a local or global label to a .kicad_sch schematic file and return a preview image.
    #[tool(description = "Add a net label (local or global) to a KiCad schematic (.kicad_sch) at a given position and return a preview image")]
    async fn add_label(
        &self,
        params: Parameters<AddLabelParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let mut content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        };

        let rotation = p.rotation.unwrap_or(0.0);
        let label_type = p.label_type.as_deref().unwrap_or("local");
        let uuid = pcb_edit::new_tstamp();

        let sexpr = if label_type == "global" {
            let shape = p.global_shape.as_deref().unwrap_or("bidirectional");
            let uuid2 = pcb_edit::new_tstamp();
            format!(
                "\n  (global_label \"{}\" (shape {}) (at {} {} {}) (fields_autoplaced)\n    (effects (font (size 1.27 1.27)) (justify left))\n    (uuid \"{}\")\n    (property \"Intersheet References\" \"\" (id 0) (at 0 0 0)\n      (effects (font (size 1.27 1.27)) hide)\n    )\n  )",
                p.text, shape, p.x, p.y, rotation, uuid2
            )
        } else {
            format!(
                "\n  (label \"{}\" (at {} {} {})\n    (effects (font (size 1.27 1.27)) (justify left))\n    (uuid \"{}\")\n  )",
                p.text, p.x, p.y, rotation, uuid
            )
        };

        if let Some(pos) = content.rfind("\n)") {
            content.insert_str(pos, &sexpr);
        } else {
            return Ok(CallToolResult::error(vec![Content::text(
                "File does not appear to be a valid KiCad schematic (no closing paren found)".to_string()
            )]));
        }

        if let Err(e) = fs::write(&path, &content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", path.display()
            ))]));
        }

        let mut items: Vec<Content> = vec![Content::text(format!(
            "{} label \"{}\" added at ({}, {})", label_type, p.text, p.x, p.y
        ))];
        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
            items.push(img);
        }
        Ok(CallToolResult::success(items))
    }

    /// Move a label in a .kicad_sch schematic file and return a preview image.
    #[tool(description = "Move a label (local or global) in a KiCad schematic (.kicad_sch) by finding its current position and updating the (at X Y ROT) clause, then return a preview image")]
    async fn move_label(
        &self,
        params: Parameters<MoveLabelParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        };

        // Build search patterns for both local and global labels at the old position.
        // We match the label text and old coordinates; rotation may be anything.
        // Pattern: (label "TEXT" (at OLD_X OLD_Y or (global_label "TEXT" (shape ...) (at OLD_X OLD_Y
        let old_x_str = format!("{}", p.old_x);
        let old_y_str = format!("{}", p.old_y);
        let new_x_str = format!("{}", p.new_x);
        let new_y_str = format!("{}", p.new_y);

        // Find the line containing `(label "TEXT" (at OLD_X OLD_Y` or
        // `(global_label "TEXT"` ... `(at OLD_X OLD_Y`
        // Strategy: scan lines, find a line matching the at-clause with old coords.
        let escaped_text = p.text.replace('"', "\\\"");
        let at_prefix = format!("(at {} {}", old_x_str, old_y_str);

        // We need to find a line that contains the label text AND the at-clause with old coords.
        // For global labels the `at` might be on the same line as the label declaration.
        // Try replacing the first occurrence of the at-clause near the label.

        // Search for the label declaration line first, then replace its (at X Y ROT)
        let mut new_content = content.clone();
        let mut replaced = false;

        // Try local label pattern: `(label "TEXT" (at OLD_X OLD_Y ROT)`
        let local_prefix = format!("(label \"{}\" (at {} {}", escaped_text, old_x_str, old_y_str);
        if let Some(start) = new_content.find(&local_prefix) {
            // Find the closing paren of the (at ...) clause
            // Find "(at " from local_prefix start
            let at_offset = local_prefix.find("(at ").unwrap();
            let abs_at = start + at_offset;
            // Find the closing ')' of the at clause
            if let Some(close) = new_content[abs_at..].find(')') {
                let abs_close = abs_at + close;
                let old_at_clause = &new_content[abs_at..=abs_close].to_string();
                // Extract current rotation from old clause: "(at X Y ROT)"
                let parts: Vec<&str> = old_at_clause.trim_start_matches('(')
                    .trim_end_matches(')')
                    .split_whitespace()
                    .collect();
                let old_rot = if parts.len() >= 4 { parts[3] } else { "0" };
                let new_rot = p.new_rotation.map(|r| r.to_string())
                    .unwrap_or_else(|| old_rot.to_string());
                let new_at_clause = format!("(at {} {} {})", new_x_str, new_y_str, new_rot);
                new_content.replace_range(abs_at..=abs_close, &new_at_clause);
                replaced = true;
            }
        }

        // Try global label pattern if local not found
        if !replaced {
            let global_prefix = format!("(global_label \"{}\"", escaped_text);
            if let Some(label_start) = new_content.find(&global_prefix) {
                // Search for `(at OLD_X OLD_Y` after the label start
                let search_region = &new_content[label_start..];
                if let Some(at_rel) = search_region.find(&at_prefix) {
                    let abs_at = label_start + at_rel;
                    if let Some(close) = new_content[abs_at..].find(')') {
                        let abs_close = abs_at + close;
                        let old_at_clause = new_content[abs_at..=abs_close].to_string();
                        let parts: Vec<&str> = old_at_clause.trim_start_matches('(')
                            .trim_end_matches(')')
                            .split_whitespace()
                            .collect();
                        let old_rot = if parts.len() >= 4 { parts[3] } else { "0" };
                        let new_rot = p.new_rotation.map(|r| r.to_string())
                            .unwrap_or_else(|| old_rot.to_string());
                        let new_at_clause = format!("(at {} {} {})", new_x_str, new_y_str, new_rot);
                        new_content.replace_range(abs_at..=abs_close, &new_at_clause);
                        replaced = true;
                    }
                }
            }
        }

        if !replaced {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Label \"{}\" at ({}, {}) not found in {}", p.text, p.old_x, p.old_y, path.display()
            ))]));
        }

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", path.display()
            ))]));
        }

        let mut items: Vec<Content> = vec![Content::text(format!(
            "Label \"{}\" moved from ({}, {}) to ({}, {})", p.text, p.old_x, p.old_y, p.new_x, p.new_y
        ))];
        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
            items.push(img);
        }
        Ok(CallToolResult::success(items))
    }

    // ---- Schematic → PCB sync -----------------------------------------------

    /// Sync footprint assignments and component values from the schematic to the PCB.
    /// Equivalent to KiCad's "Update PCB from Schematic" (F8).
    /// Does NOT reposition components or reroute traces — only syncs metadata.
    #[tool(description = "Sync footprint names and component values from a .kicad_sch schematic to a .kicad_pcb file — equivalent to KiCad's 'Update PCB from Schematic' (F8). Reports what was changed, what is in the schematic but missing from the PCB, and vice-versa.")]
    async fn update_pcb_from_schematic(
        &self,
        params: Parameters<UpdatePcbFromSchParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let dry_run = p.dry_run.unwrap_or(false);

        // 1. Export netlist to a temp file
        let netlist_path = std::env::temp_dir().join(format!(
            "kicad_netlist_{}.net",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let (_, stderr, code) = run_kicad_cli(&[
            "sch", "export", "netlist",
            "--output", netlist_path.to_str().unwrap_or("/tmp/netlist.net"),
            "--format", "kicadsexpr",
            &p.sch_path,
        ]).await?;

        let netlist_content = match fs::read_to_string(&netlist_path).await {
            Ok(c) => { let _ = fs::remove_file(&netlist_path).await; c }
            Err(_) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to export netlist (exit {code}):\n{stderr}"
            ))])),
        };

        // 2. Parse netlist and extract ref → (footprint, value)
        let nodes = match crate::parser::sexp::parse_sexp(&netlist_content) {
            Ok(n) => n,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to parse netlist S-expression: {e}"
            ))])),
        };

        // Walk (export (components (comp ...)))
        let mut sch_map: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();

        'outer: for top in &nodes {
            // top should be the (export ...) node
            if let Some(export) = top.get_child("components") {
                if let Some(items) = export.as_list() {
                    for item in items {
                        // each item is (comp (ref "X") (value "Y") (footprint "Z") ...)
                        if let Some(children) = item.as_list() {
                            if children.first().and_then(|n| n.as_atom()) != Some("comp") {
                                continue;
                            }
                            let ref_val = item.get_child("ref")
                                .and_then(|n| n.nth(1))
                                .and_then(|n| n.as_atom())
                                .map(|s| s.to_string());
                            let value = item.get_child("value")
                                .and_then(|n| n.nth(1))
                                .and_then(|n| n.as_atom())
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            let footprint = item.get_child("footprint")
                                .and_then(|n| n.nth(1))
                                .and_then(|n| n.as_atom())
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            if let Some(r) = ref_val {
                                sch_map.insert(r, (footprint, value));
                            }
                        }
                    }
                }
                break 'outer;
            }
        }

        if sch_map.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(
                "No components found in netlist. Is the schematic populated?".to_string()
            )]));
        }

        // 3. Read PCB
        let pcb_path = PathBuf::from(&p.pcb_path);
        let _guard = self.lock_file(&pcb_path).await;
        let mut pcb_content = match fs::read_to_string(&pcb_path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read PCB file: {e}"
            ))])),
        };

        // 4. Find footprint blocks in PCB
        let pcb_blocks = pcb_edit::find_footprint_blocks(&pcb_content);

        // 5. Compare and build changes
        let mut updated: Vec<String> = Vec::new();
        let mut missing_from_pcb: Vec<String> = Vec::new();
        let mut no_change: Vec<String> = Vec::new();

        // Collect refs that need updating (process in reverse byte order to preserve offsets)
        let mut pending: Vec<(String, String, String)> = Vec::new(); // (ref, new_fp, new_val)

        for (reference, (sch_fp, sch_val)) in &sch_map {
            match pcb_blocks.get(reference) {
                None => {
                    missing_from_pcb.push(reference.clone());
                }
                Some(range) => {
                    let block = &pcb_content[range.clone()];
                    let pcb_fp = pcb_edit::extract_fp_name(block).unwrap_or_default();
                    let pcb_val = pcb_edit::fp_text_value(block, "value").unwrap_or_default();

                    let fp_differs = !sch_fp.is_empty() && sch_fp != &pcb_fp;
                    let val_differs = !sch_val.is_empty() && sch_val != &pcb_val;

                    if fp_differs || val_differs {
                        pending.push((reference.clone(), sch_fp.clone(), sch_val.clone()));
                        let mut change = format!("  {reference}:");
                        if fp_differs {
                            change.push_str(&format!("\n    footprint: {pcb_fp:?} → {sch_fp:?}"));
                        }
                        if val_differs {
                            change.push_str(&format!("\n    value:     {pcb_val:?} → {sch_val:?}"));
                        }
                        updated.push(change);
                    } else {
                        no_change.push(reference.clone());
                    }
                }
            }
        }

        // Find PCB refs not in schematic
        let mut missing_from_sch: Vec<String> = pcb_blocks
            .keys()
            .filter(|r| !sch_map.contains_key(*r))
            .cloned()
            .collect();
        missing_from_sch.sort();
        missing_from_pcb.sort();

        // 6. Apply changes (if not dry_run)
        if !dry_run && !pending.is_empty() {
            // Re-find blocks for each ref just before modifying (content changes after each apply)
            for (reference, sch_fp, sch_val) in &pending {
                let blocks = pcb_edit::find_footprint_blocks(&pcb_content);
                if let Some(range) = blocks.get(reference) {
                    let block = pcb_content[range.clone()].to_string();
                    let new_block = {
                        let b = if !sch_fp.is_empty() {
                            pcb_edit::replace_fp_name(&block, sch_fp)
                        } else {
                            block.clone()
                        };
                        if !sch_val.is_empty() {
                            pcb_edit::replace_value(&b, sch_val)
                        } else {
                            b
                        }
                    };
                    let end = range.end;
                    let start = range.start;
                    pcb_content.replace_range(start..end, &new_block);
                }
            }

            // Write updated PCB
            if let Err(e) = fs::write(&pcb_path, &pcb_content).await {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to write PCB file: {e}"
                ))]));
            }
        }

        // 6b. Assign nets to pads using pcbnew Python API.
        // Parse the (nets ...) section of the netlist to build {ref: {pin: net}} map,
        // then use pcbnew to set net assignments on each pad. This is what makes the
        // autorouter actually have a ratsnest to route.
        let net_assignment_report = if !dry_run {
            // Build ref → pin → net map by scanning the raw netlist text.
            // The kicadsexpr format is:
            //   (net (code "N") (name "NET_NAME") (class "Default")
            //     (node (ref "U1") (pin "4") (pinfunction "...") (pintype "..."))
            //     ...)
            // We parse with simple string scanning rather than the sexp tree to
            // avoid tree-traversal bugs with variable nesting depth.
            let mut pin_nets: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
                std::collections::HashMap::new();

            let nets_start = netlist_content.find("(nets").unwrap_or(netlist_content.len());
            let nets_section = &netlist_content[nets_start..];
            let mut scan = 0;
            while let Some(net_rel) = nets_section[scan..].find("(net\n").or_else(|| nets_section[scan..].find("(net\r")) {
                let net_start = scan + net_rel;
                let net_end = pcb_edit::block_end(nets_section, net_start);
                let net_block = &nets_section[net_start..net_end];

                // Extract net name: (name "NET_NAME")
                let net_name = if let Some(np) = net_block.find("(name \"") {
                    let after = &net_block[np + 7..];
                    after[..after.find('"').unwrap_or(0)].to_string()
                } else { scan = net_end; continue; };

                // Extract all (node (ref "X") (pin "Y") ...) entries
                let mut node_scan = 0;
                while let Some(node_rel) = net_block[node_scan..].find("(node") {
                    let node_start = node_scan + node_rel;
                    let node_end = pcb_edit::block_end(net_block, node_start);
                    let node_block = &net_block[node_start..node_end];

                    let ref_val = node_block.find("(ref \"").map(|p| {
                        let after = &node_block[p + 6..];
                        after[..after.find('"').unwrap_or(0)].to_string()
                    });
                    let pin_val = node_block.find("(pin \"").map(|p| {
                        let after = &node_block[p + 6..];
                        after[..after.find('"').unwrap_or(0)].to_string()
                    });

                    if let (Some(r), Some(pin)) = (ref_val, pin_val) {
                        pin_nets.entry(r).or_default().insert(pin, net_name.clone());
                    }
                    node_scan = node_end;
                }
                scan = net_end;
            }

            if pin_nets.is_empty() {
                "No net data found in netlist — pad nets not assigned.".to_string()
            } else {
                // Serialize pin_nets as JSON and pass to Python
                let pin_nets_json = serde_json::to_string(&pin_nets).unwrap_or_default();
                let pcb_path_str = p.pcb_path.clone();
                let net_script = format!(r#"
import pcbnew, json, sys
board = pcbnew.LoadBoard({pcb:?})
pin_nets = json.loads({nets})

# Build ref → list of footprints. When there are duplicates (ghost from a failed
# replace_footprint), keep all of them — assign nets to every instance so nothing
# is left floating regardless of placement method.
fps_by_ref = {{}}
for fp in board.GetFootprints():
    fps_by_ref.setdefault(fp.GetReference(), []).append(fp)

changed = []
for ref, fp_list in fps_by_ref.items():
    if ref not in pin_nets:
        continue
    for fp in fp_list:
        for pad in fp.Pads():
            pad_num = pad.GetNumber()
            if pad_num not in pin_nets[ref]:
                continue
            net_name = pin_nets[ref][pad_num]
            net = board.FindNet(net_name)
            if net is None:
                net = pcbnew.NETINFO_ITEM(board, net_name)
                board.Add(net)
            current = pad.GetNetname()
            if current != net_name and (current == "" or current.startswith("unconnected-")):
                pad.SetNet(net)
                changed.append(f"{{ref}} pad {{pad_num}} → {{net_name}}")
board.BuildListOfNets()
board.Save({pcb:?})
print(json.dumps({{"changed": changed}}))
"#,
                    pcb = pcb_path_str,
                    nets = serde_json::Value::String(pin_nets_json),
                );

                let py_out = Command::new("python3")
                    .args(["-c", &net_script])
                    .env("DISPLAY", "")
                    .output().await;

                match py_out {
                    Ok(out) if out.status.success() => {
                        let res: serde_json::Value = serde_json::from_str(
                            &String::from_utf8_lossy(&out.stdout)
                        ).unwrap_or_default();
                        let n = res["changed"].as_array().map(|a| a.len()).unwrap_or(0);
                        format!("Net assignment: {n} pad(s) assigned.")
                    }
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        format!("Net assignment warning: {err}")
                    }
                    Err(e) => format!("Net assignment skipped (python3 unavailable): {e}"),
                }
            }
        } else {
            String::new()
        };

        // 7. Build report
        let mut report = String::new();
        if dry_run { report.push_str("DRY RUN — no changes written.\n\n"); }

        if pending.is_empty() {
            report.push_str("All schematic components already match the PCB. No changes needed.\n");
        } else {
            report.push_str(&format!(
                "{} component(s) {}:\n{}\n",
                pending.len(),
                if dry_run { "would be updated" } else { "updated" },
                updated.join("\n")
            ));
        }

        if !missing_from_pcb.is_empty() {
            report.push_str(&format!(
                "\n{} ref(s) in schematic but NOT in PCB (add them manually):\n  {}\n",
                missing_from_pcb.len(),
                missing_from_pcb.join(", ")
            ));
        }
        if !missing_from_sch.is_empty() {
            report.push_str(&format!(
                "\n{} ref(s) in PCB but NOT in schematic:\n  {}\n",
                missing_from_sch.len(),
                missing_from_sch.join(", ")
            ));
        }
        if !no_change.is_empty() {
            no_change.sort();
            report.push_str(&format!(
                "\n{} component(s) already in sync: {}\n",
                no_change.len(),
                no_change.join(", ")
            ));
        }

        if !net_assignment_report.is_empty() {
            report.push_str(&format!("\n{net_assignment_report}\n"));
        }

        let mut items = vec![Content::text(report)];
        if !dry_run && (!pending.is_empty() || !net_assignment_report.is_empty()) {
            items.extend(self.render_board(&p.pcb_path).await);
        }
        Ok(CallToolResult::success(items))
    }

    // ---- Schematic symbol replacement --------------------------------------

    /// Replace a symbol instance in a schematic (.kicad_sch) with a new symbol.
    /// Swaps lib_id, updates Value and Footprint properties, remaps pin UUIDs by pin name
    /// so existing wire connections are preserved where pin names match.
    /// Updates the lib_symbols section with the new symbol definition if found locally.
    #[tool(description = "Replace a symbol instance in a .kicad_sch schematic file with a new symbol. Updates lib_id, Value, Footprint properties and remaps pin UUIDs by name to preserve wire connections. Pass new_lib_id as 'library:symbol'. Returns a preview render.")]
    async fn replace_symbol(
        &self,
        params: Parameters<ReplaceSymbolParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", p.path
            ))])),
        };

        // --- Find the symbol instance block ---
        let blocks = find_symbol_blocks(&content);
        let inst_range = match blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Reference '{}' not found in {}.", p.reference, p.path
            ))])),
        };
        let old_block = &content[inst_range.clone()];

        // Extract old lib_id
        let old_lib_id = {
            let prefix = "(symbol (lib_id \"";
            let pos = old_block.find(prefix).unwrap_or(0);
            let after = &old_block[pos + prefix.len()..];
            after[..after.find('"').unwrap_or(0)].to_string()
        };

        // Extract existing properties to keep
        let old_value = sch_property_value(old_block, "Value")
            .unwrap_or_else(|| p.reference.clone());
        let old_footprint = sch_property_value(old_block, "Footprint")
            .unwrap_or_default();

        let new_value = p.new_value.as_deref().unwrap_or(&old_value);
        let new_footprint = p.new_footprint.as_deref().unwrap_or(&old_footprint);

        // --- Find lib_symbols section ---
        // Plain substring search (not needing 2-space/tab indent matching): there's
        // only ever one top-level `(lib_symbols` section per schematic, so this is
        // unambiguous regardless of file indentation style.
        let lib_sym_start = match content.find("(lib_symbols") {
            Some(s) => s,
            None => return Ok(CallToolResult::error(vec![Content::text(
                "No lib_symbols section found in schematic.".to_string()
            )])),
        };
        let lib_sym_end = pcb_edit::block_end(&content, lib_sym_start);
        let lib_symbols_block = &content[lib_sym_start..lib_sym_end];

        // --- Extract pin name maps ---
        let old_pins_by_num = extract_symbol_pins(lib_symbols_block, &old_lib_id);
        let old_instance_pins = extract_instance_pins(old_block);

        // --- Find new symbol definition ---
        // First check if it's already in lib_symbols
        let new_sym_def_in_lib = find_lib_symbol_def(lib_symbols_block, &p.new_lib_id);

        // Also look for a .kicad_sym file in the schematic's directory
        let sch_dir = path.parent().unwrap_or(Path::new("."));
        let new_sym_def_from_file: Option<String> = {
            let mut found = None;
            // new_lib_id is "library:symbol" — the library is the filename stem
            let (lib_name, _sym_name) = p.new_lib_id.split_once(':').unwrap_or(("", &p.new_lib_id));
            let sym_file = sch_dir.join(format!("{}.kicad_sym", lib_name));
            if sym_file.exists() {
                if let Ok(sym_content) = fs::read_to_string(&sym_file).await {
                    // Find the symbol block for our specific symbol
                    if let Some(range) = find_lib_symbol_def(&sym_content, &p.new_lib_id) {
                        found = Some(sym_content[range].to_string());
                    } else {
                        // Try matching just the symbol name part
                        let sym_name = p.new_lib_id.split_once(':').map(|(_, s)| s).unwrap_or(&p.new_lib_id);
                        let marker = format!("(symbol \"{}\"", sym_name);
                        if let Some(s) = sym_content.find(&marker) {
                            let e = pcb_edit::block_end(&sym_content, s);
                            found = Some(sym_content[s..e].to_string());
                        }
                    }
                }
            }
            found
        };

        // Determine the new symbol definition text (for lib_symbols)
        let new_sym_def_text: Option<String> = new_sym_def_from_file.or_else(|| {
            new_sym_def_in_lib.as_ref().map(|range| lib_symbols_block[range.clone()].to_string())
        });

        // Extract new pin names from the new symbol definition
        let new_pins_by_num: std::collections::HashMap<String, String> = if let Some(ref def) = new_sym_def_text {
            // The def is just the symbol block — try with full lib_id first, then bare name
            let by_full = extract_symbol_pins(def, &p.new_lib_id);
            if !by_full.is_empty() {
                by_full
            } else {
                let sym_name = p.new_lib_id.split_once(':').map(|(_, s)| s).unwrap_or(&p.new_lib_id);
                extract_symbol_pins(def, sym_name)
            }
        } else {
            std::collections::HashMap::new()
        };

        // --- Remap pin UUIDs ---
        let (new_instance_pins, dangling_pins) = if !new_pins_by_num.is_empty() {
            remap_pin_uuids(&old_pins_by_num, &new_pins_by_num, &old_instance_pins)
        } else {
            // No pin info — generate fresh UUIDs for all old instance pins
            let fresh: Vec<(String, String)> = old_instance_pins
                .iter()
                .map(|(num, _)| (num.clone(), pcb_edit::new_tstamp()))
                .collect();
            (fresh, vec![])
        };

        // --- Build the new instance block ---
        let new_block = rebuild_symbol_instance(old_block, &p.new_lib_id, new_value, new_footprint, &new_instance_pins);

        // --- Update lib_symbols section ---
        // Remove old symbol def and inject new one (if we have it)
        let new_lib_symbols_block = {
            let mut inner = lib_symbols_block.to_string(); // "(lib_symbols\n  ..."

            // Remove old symbol def if it differs from new
            if old_lib_id != p.new_lib_id {
                if let Some(old_range) = find_lib_symbol_def(&inner, &old_lib_id) {
                    // Also remove any preceding newline + indent
                    let remove_start = if old_range.start >= 3 && &inner[old_range.start-3..old_range.start] == "\n  " {
                        old_range.start - 3
                    } else {
                        old_range.start
                    };
                    inner = format!("{}{}", &inner[..remove_start], &inner[old_range.end..]);
                }
            }

            // Inject new symbol def if not already present
            if find_lib_symbol_def(&inner, &p.new_lib_id).is_none() {
                if let Some(def) = &new_sym_def_text {
                    // Inject before the closing ')' of lib_symbols
                    let close = inner.rfind(')').unwrap_or(inner.len());
                    // Rewrite the def with lib_id as prefix if needed
                    let def_to_inject = if def.starts_with(&format!("(symbol \"{}\"", p.new_lib_id)) {
                        def.clone()
                    } else {
                        // Bare symbol name — prefix it
                        let sym_name = p.new_lib_id.split_once(':').map(|(_, s)| s).unwrap_or(&p.new_lib_id);
                        let bare_marker = format!("(symbol \"{}\"", sym_name);
                        let full_marker = format!("(symbol \"{}\"", p.new_lib_id);
                        def.replacen(&bare_marker, &full_marker, 1)
                    };
                    inner = format!("{}\n  {}\n{}", &inner[..close], def_to_inject, &inner[close..]);
                }
            }
            inner
        };

        // Reconstruct full file content
        let new_content = format!(
            "{}{}{}{}{}",
            &content[..lib_sym_start],
            new_lib_symbols_block,
            &content[lib_sym_end..inst_range.start],
            new_block,
            &content[inst_range.end..]
        );

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", p.path
            ))]));
        }

        let mut summary = format!(
            "Replaced symbol {} in {}:\n  lib_id: {} → {}\n  value: {} → {}\n  footprint: {} → {}\n  pins remapped: {}",
            p.reference, p.path,
            old_lib_id, p.new_lib_id,
            old_value, new_value,
            old_footprint, new_footprint,
            new_instance_pins.len(),
        );
        if !dangling_pins.is_empty() {
            summary.push_str(&format!(
                "\n  WARNING: {} pin(s) from old symbol have no match in new symbol — wires may be dangling: {}",
                dangling_pins.len(),
                dangling_pins.join(", ")
            ));
        }
        if new_pins_by_num.is_empty() {
            summary.push_str("\n  WARNING: Could not find new symbol definition — all pin UUIDs are fresh. Check wire connections.");
        }

        let mut contents = vec![Content::text(summary)];
        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
            contents.push(img);
        }
        Ok(CallToolResult::success(contents))
    }

    // ---- Trace cleanup -----------------------------------------------------

    /// Remove orphaned (dangling) trace segments from a .kicad_pcb file.
    /// A segment is orphaned if its connected component has no pad endpoint.
    #[tool(description = "Remove orphaned/dangling trace segments from a .kicad_pcb file. Segments whose connected component has no pad connection at any endpoint are deleted.")]
    async fn cleanup_traces(
        &self,
        params: Parameters<CleanupTracesParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let dry_run = p.dry_run.unwrap_or(false);

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        };

        let total_before = parse_segments(&content).len();

        // Build layer filter
        let layer_strings: Vec<String> = p.layers
            .as_deref()
            .map(|l| l.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        let layer_filter: Option<Vec<&str>> = if layer_strings.is_empty() {
            None
        } else {
            Some(layer_strings.iter().map(String::as_str).collect())
        };

        let (new_content, removed, layer_counts) =
            remove_orphaned_segments(&content, layer_filter.as_deref());

        let total_after = total_before - removed;

        let mut summary = format!(
            "{} segments scanned, {} orphaned segments {} on layers:\n",
            total_before,
            removed,
            if dry_run { "would be removed" } else { "removed" },
        );

        if layer_counts.is_empty() {
            summary.push_str("  (none)\n");
        } else {
            let mut sorted: Vec<_> = layer_counts.iter().collect();
            sorted.sort_by_key(|(k, _)| (*k).clone());
            for (layer, count) in sorted {
                summary.push_str(&format!("  {}: {}\n", layer, count));
            }
        }
        summary.push_str(&format!("Total segments: {} → {}", total_before, total_after));

        if dry_run || removed == 0 {
            return Ok(CallToolResult::success(vec![Content::text(summary)]));
        }

        let _guard = self.lock_file(&path).await;
        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to write {}: {e}", path.display()
            ))]));
        }

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    // ---- PCB board outline, zones, traces, graphics ------------------------

    /// Return the board outline bounding box parsed from Edge.Cuts gr_line elements.
    #[tool(description = "Return the board outline bounding box (x_min, y_min, x_max, y_max) from Edge.Cuts gr_line elements — use this before set_board_outline or placing components to know current board dimensions")]
    async fn get_board_outline(
        &self,
        params: Parameters<GetBoardOutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let content = match fs::read_to_string(&params.0.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };
        let (x_min, y_min, x_max, y_max) = parse_edge_cuts_bounds(&content);
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Board outline:\n  x_min={x_min}\n  y_min={y_min}\n  x_max={x_max}\n  y_max={y_max}\n  width={:.4}mm\n  height={:.4}mm",
            x_max - x_min, y_max - y_min
        ))]))
    }

    /// Replace all Edge.Cuts gr_line elements with a simple rectangle and optionally
    /// resize the first copper fill zone polygon to match.
    #[tool(description = "Set the PCB board outline to a rectangle defined by x_min/y_min/x_max/y_max — replaces all Edge.Cuts lines and optionally updates the copper fill zone polygon. Returns a render.")]
    async fn set_board_outline(
        &self,
        params: Parameters<SetBoardOutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        // Capture old board bounds before modifying Edge.Cuts (used to match zones)
        let old_bounds = parse_edge_cuts_bounds(&content);

        // Remove existing Edge.Cuts gr_line / gr_rect blocks
        let content = remove_edge_cuts_lines(&content);

        // Build four new gr_line segments
        let ts = [pcb_edit::new_tstamp(), pcb_edit::new_tstamp(),
                  pcb_edit::new_tstamp(), pcb_edit::new_tstamp()];
        let new_lines = format!(
            "  (gr_line (start {x1} {y1}) (end {x2} {y1})\n    (stroke (width 0.05) (type solid)) (layer \"Edge.Cuts\") (tstamp {t0}))\n\
             \n  (gr_line (start {x2} {y1}) (end {x2} {y2})\n    (stroke (width 0.05) (type solid)) (layer \"Edge.Cuts\") (tstamp {t1}))\n\
             \n  (gr_line (start {x2} {y2}) (end {x1} {y2})\n    (stroke (width 0.05) (type solid)) (layer \"Edge.Cuts\") (tstamp {t2}))\n\
             \n  (gr_line (start {x1} {y2}) (end {x1} {y1})\n    (stroke (width 0.05) (type solid)) (layer \"Edge.Cuts\") (tstamp {t3}))",
            x1=p.x_min, y1=p.y_min, x2=p.x_max, y2=p.y_max,
            t0=ts[0], t1=ts[1], t2=ts[2], t3=ts[3]
        );

        let insert_pos = content.rfind("\n)").unwrap_or(content.len());
        let mut content = format!("{}\n{}\n{}", &content[..insert_pos], new_lines, &content[insert_pos..]);

        // Update ALL zone polygons that match the old board outline, not just the first.
        if p.update_zones.unwrap_or(true) {
            let new_bounds = (p.x_min, p.y_min, p.x_max, p.y_max);
            content = update_all_zone_polygons(&content, old_bounds, new_bounds);
        }

        if let Err(e) = fs::write(&path, &content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        // Always refill zones after resizing the board outline.
        let tmp = std::env::temp_dir().join(format!("kicad_outline_fill_{}.json",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis()));
        let _ = run_kicad_cli(&[
            "pcb", "drc",
            "--output", tmp.to_str().unwrap_or("/tmp/fill.json"),
            "--format", "json",
            "--refill-zones", "--save-board",
            &p.path,
        ]).await;
        let _ = fs::remove_file(&tmp).await;

        let summary = format!(
            "Board outline set to ({}, {}) → ({}, {}), size {:.3}×{:.3}mm (zones updated and refilled)",
            p.x_min, p.y_min, p.x_max, p.y_max,
            p.x_max - p.x_min, p.y_max - p.y_min
        );
        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Force-refill all copper pour zones without running a full DRC.
    /// Uses kicad-cli pcb drc --refill-zones --save-board under the hood.
    #[tool(description = "Force-refill all copper pour zones in a .kicad_pcb file — equivalent to KiCad's Edit → Fill All Zones. Returns a render after fill.")]
    async fn fill_zones(
        &self,
        params: Parameters<FillZonesParams>,
    ) -> Result<CallToolResult, McpError> {
        let tmp = std::env::temp_dir().join(format!("kicad_drc_fill_{}.json",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis()));

        let (_, stderr, code) = run_kicad_cli(&[
            "pcb", "drc",
            "--output", tmp.to_str().unwrap_or("/tmp/drc_fill.json"),
            "--format", "json",
            "--refill-zones",
            "--save-board",
            &params.0.path,
        ]).await?;

        let _ = fs::remove_file(&tmp).await;

        if code != 0 && !stderr.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Zone fill failed (exit {code}):\n{stderr}"
            ))]));
        }

        let mut contents = vec![Content::text(format!("Zones filled and board saved: {}", params.0.path))];
        contents.extend(self.render_board(&params.0.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Add a copper trace segment to a PCB file.
    #[tool(description = "Add a copper trace segment to a .kicad_pcb file between two points on a given layer. Pass check=true to run the same collision check as check_trace_clearance before writing (recommended for routing sessions) — any COLLISION aborts the write unless force=true is also set. Returns a render.")]
    async fn add_trace(
        &self,
        params: Parameters<AddTraceParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let width = p.width.unwrap_or(0.25);
        let net = p.net.as_deref().unwrap_or("").to_string();

        if p.check.unwrap_or(false) {
            let (collisions, warnings) = compute_clearance(
                &content, p.x1, p.y1, p.x2, p.y2, &p.layer, width, 0.1, &net,
            );
            if !collisions.is_empty() && !p.force.unwrap_or(false) {
                let mut output = format!(
                    "Refused to add trace: {} COLLISION(S) found. Pass force=true to write anyway.\n\n",
                    collisions.len()
                );
                output.push_str(&format!("COLLISIONS ({}):\n", collisions.len()));
                for c in &collisions { output.push_str(c); output.push('\n'); }
                if !warnings.is_empty() {
                    output.push_str(&format!("\nCLEARANCE WARNINGS ({}):\n", warnings.len()));
                    for w in &warnings { output.push_str(w); output.push('\n'); }
                }
                return Ok(CallToolResult::error(vec![Content::text(output)]));
            }
        }

        let ts = pcb_edit::new_tstamp();

        let segment = format!(
            "\t(segment\n\t\t(start {} {})\n\t\t(end {} {})\n\t\t(width {})\n\t\t(layer \"{}\")\n\t\t(net \"{}\")\n\t\t(uuid \"{}\")\n\t)",
            p.x1, p.y1, p.x2, p.y2, width, p.layer, net, ts
        );

        let insert_pos = content.rfind("\n)").unwrap_or(content.len());
        let new_content = format!("{}\n{}\n{}", &content[..insert_pos], segment, &content[insert_pos..]);

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let summary = format!("Added trace ({},{})→({},{}) on {} width={}mm net={}",
            p.x1, p.y1, p.x2, p.y2, p.layer, width, net);
        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Delete gr_text, gr_line, or gr_rect elements from a PCB, filtered by text content,
    /// layer, or tstamp. Optionally also removes matching footprint blocks.
    #[tool(description = "Delete graphic elements (gr_text, gr_line, gr_rect) from a .kicad_pcb by text content, layer, or tstamp. Use include_footprints=true to also remove footprints matching by reference/value. Returns a render.")]
    async fn delete_graphic(
        &self,
        params: Parameters<DeleteGraphicParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        if p.text_contains.is_none() && p.layer.is_none() && p.tstamp.is_none() {
            return Ok(CallToolResult::error(vec![Content::text(
                "Provide at least one filter: text_contains, layer, or tstamp".to_string()
            )]));
        }

        let (new_content, removed) = remove_matching_graphics(
            &content,
            p.text_contains.as_deref(),
            p.layer.as_deref(),
            p.tstamp.as_deref(),
            p.include_footprints.unwrap_or(false),
        );

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let mut contents = vec![Content::text(format!("Removed {removed} graphic element(s)."))];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Delete copper trace segments from a PCB, filtered by net, layer, uuid, and/or region.
    /// Replaces the ad-hoc script previously needed to undo a bad routing pass.
    #[tool(description = "Delete copper trace segments from a .kicad_pcb, filtered by net name, layer, uuid, and/or a bounding region (all provided filters combine as AND). Use dry_run=true first to preview matches before writing — especially for a net/layer/region filter that could match more than expected. Use this instead of a throwaway script when traces were routed incorrectly. Returns count removed (or previewed) and an updated render.")]
    async fn delete_trace(
        &self,
        params: Parameters<DeleteTraceParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let has_region = p.x1.is_some() && p.y1.is_some() && p.x2.is_some() && p.y2.is_some();
        if p.net.is_none() && p.layer.is_none() && p.uuid.is_none() && !has_region {
            return Ok(CallToolResult::error(vec![Content::text(
                "Provide at least one filter: net, layer, uuid, or all four of x1/y1/x2/y2".to_string()
            )]));
        }

        let region = if has_region {
            let (x1, y1, x2, y2) = (p.x1.unwrap(), p.y1.unwrap(), p.x2.unwrap(), p.y2.unwrap());
            Some((x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)))
        } else {
            None
        };

        let matches = find_matching_traces(&content, p.net.as_deref(), p.layer.as_deref(), p.uuid.as_deref(), region);

        if matches.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No matching traces found; nothing removed.".to_string()
            )]));
        }

        if p.dry_run.unwrap_or(false) {
            let listing = matches.iter()
                .map(|(_, desc)| format!("  {desc}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "DRY RUN — would remove {} trace segment(s), nothing written:\n{}",
                matches.len(), listing
            ))]));
        }

        let removed = matches.len();
        let ranges: Vec<std::ops::Range<usize>> = matches.into_iter().map(|(r, _)| r).collect();
        let new_content = splice_out_ranges(&content, ranges);

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let mut contents = vec![Content::text(format!("Removed {removed} trace segment(s)."))];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Add a graphic element (text, line, rect) to a PCB file.
    #[tool(description = "Add a graphic element (text, line, rect, circle) to a .kicad_pcb file on a specified layer. Returns a render.")]
    async fn add_graphic(
        &self,
        params: Parameters<AddGraphicParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let layer = p.layer.as_deref().unwrap_or("F.SilkS");
        let width = p.width.unwrap_or(0.12);
        let ts = pcb_edit::new_tstamp();
        let gtype = p.graphic_type.as_deref().unwrap_or("text");

        let element = match gtype {
            "line" => {
                let (x2, y2) = (p.x2.unwrap_or(p.x), p.y2.unwrap_or(p.y));
                format!(
                    "  (gr_line (start {} {}) (end {} {})\n    (stroke (width {}) (type solid)) (layer \"{}\") (tstamp {}))",
                    p.x, p.y, x2, y2, width, layer, ts
                )
            }
            "rect" => {
                let (x2, y2) = (p.x2.unwrap_or(p.x), p.y2.unwrap_or(p.y));
                format!(
                    "  (gr_rect (start {} {}) (end {} {})\n    (stroke (width {}) (type solid)) (fill none) (layer \"{}\") (tstamp {}))",
                    p.x, p.y, x2, y2, width, layer, ts
                )
            }
            "circle" => {
                let radius = p.x2.unwrap_or(1.0);
                format!(
                    "  (gr_circle (center {} {}) (end {} {})\n    (stroke (width {}) (type solid)) (fill none) (layer \"{}\") (tstamp {}))",
                    p.x, p.y, p.x + radius, p.y, width, layer, ts
                )
            }
            _ => { // "text"
                let text = p.text.as_deref().unwrap_or("TEXT");
                let font_size = p.font_size.unwrap_or(1.0);
                let rot = p.rotation.unwrap_or(0.0);
                let rot_str = if rot.abs() < 0.001 { String::new() } else { format!(" {}", rot) };
                format!(
                    "  (gr_text \"{}\" (at {}{} {})\n    (effects (font (size {} {}) (thickness {})))\n    (layer \"{}\") (tstamp {}))",
                    text, p.x, rot_str, p.y, font_size, font_size, width, layer, ts
                )
            }
        };

        let insert_pos = content.rfind("\n)").unwrap_or(content.len());
        let new_content = format!("{}\n{}\n{}", &content[..insert_pos], element, &content[insert_pos..]);

        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let mut contents = vec![Content::text(format!("Added {gtype} on {layer} at ({}, {})", p.x, p.y))];
        contents.extend(self.render_board(&p.path).await);
        Ok(CallToolResult::success(contents))
    }

    /// Return the absolute board coordinates of pads for a given component reference.
    /// Takes the footprint placement (position + rotation) into account.
    #[tool(description = "Return the absolute board coordinates of one or all pads of a component (e.g. get_pad_position U1 pad=5). Accounts for footprint rotation. Use this instead of computing pad positions manually.")]
    async fn get_pad_position(
        &self,
        params: Parameters<GetPadPositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let blocks = pcb_edit::find_footprint_blocks(&content);
        let range = match blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Reference '{}' not found.", p.reference
            ))])),
        };

        let block = &content[range];
        let pads = extract_pad_positions(block);

        let result = if let Some(pad_num) = &p.pad {
            let matches: Vec<_> = pads.iter().filter(|(n, _, _, _)| n == pad_num).collect();
            if matches.is_empty() {
                format!("Pad {} not found in {}", pad_num, p.reference)
            } else {
                matches.iter().map(|(num, x, y, layer)| {
                    format!("Pad {} of {} [{}]: ({:.4}, {:.4})", num, p.reference, layer, x, y)
                }).collect::<Vec<_>>().join("\n")
            }
        } else {
            let mut lines = vec![format!("Pads for {}:", p.reference)];
            for (num, x, y, layer) in &pads {
                lines.push(format!("  pad {num}: ({x:.4}, {y:.4}) [{layer}]"));
            }
            lines.join("\n")
        };

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    // ---- Schematic symbol placement ----------------------------------------

    /// Move a schematic symbol instance to new coordinates.
    #[tool(description = "Move a schematic symbol instance to new coordinates in .kicad_sch. Returns a render preview.")]
    async fn move_symbol(
        &self,
        params: Parameters<MoveSymbolParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        // Find the symbol instance block by reference
        let range = match find_sch_symbol_by_ref(&content, &p.reference) {
            Some(r) => r,
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Symbol '{}' not found in schematic.", p.reference
            ))])),
        };

        let old_block = &content[range.clone()];
        // The (at X Y ROT) is the second token on the opening line of the symbol block
        let new_block = sch_replace_at(old_block, p.x, p.y, p.rotation);

        let new_content = format!("{}{}{}", &content[..range.start], new_block, &content[range.end..]);
        if let Err(e) = fs::write(&path, &new_content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let mut contents = vec![Content::text(format!("Moved {} to ({}, {})", p.reference, p.x, p.y))];
        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await { contents.push(img); }
        Ok(CallToolResult::success(contents))
    }

    /// Return the schematic canvas coordinates of pins for a given symbol reference.
    /// Accounts for symbol placement position and rotation.
    #[tool(description = "Return the schematic canvas coordinates of one or all pins of a symbol (e.g. get_pin_position U1 pin=5). Use this to place wires and labels at the correct attachment points.")]
    async fn get_pin_position(
        &self,
        params: Parameters<GetPinPositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let pins = compute_pin_positions(&content, &p.reference);
        if pins.is_empty() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Symbol '{}' not found or has no pins in lib_symbols.", p.reference
            ))]));
        }

        let result = if let Some(pin_num) = &p.pin {
            match pins.iter().find(|(n, _, _, _)| n == pin_num) {
                Some((num, name, x, y)) => format!(
                    "Pin {} (\"{}\") of {}: ({:.4}, {:.4})", num, name, p.reference, x, y
                ),
                None => format!("Pin {} not found in {}", pin_num, p.reference),
            }
        } else {
            let mut lines = vec![format!("Pins for {}:", p.reference)];
            for (num, name, x, y) in &pins {
                lines.push(format!("  pin {} \"{}\": ({:.4}, {:.4})", num, name, x, y));
            }
            lines.join("\n")
        };

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    // ---- Autorouter --------------------------------------------------------

    /// Autoroute a PCB using FreeRouting. Exports a Specctra DSN via pcbnew Python API,
    /// runs FreeRouting headlessly, imports the SES result back, and saves the board.
    /// Requires the FreeRouting JAR at ~/.local/share/freerouting.jar (or specify freerouting_jar).
    #[tool(description = "Autoroute a .kicad_pcb file using FreeRouting — exports DSN, runs FreeRouting headlessly, imports SES routes back, saves the board, and returns a render. Requires FreeRouting JAR at ~/.local/share/freerouting.jar.")]
    async fn autoroute_pcb(
        &self,
        params: Parameters<AutoroutePcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let max_passes = p.max_passes.unwrap_or(40);
        let jar = p.freerouting_jar.unwrap_or_else(|| {
            std::env::var("HOME").unwrap_or_default() + "/.local/share/freerouting.jar"
        });
        let save = p.save.unwrap_or(true);

        if !PathBuf::from(&jar).exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "FreeRouting JAR not found at {jar}.\n\
                 Download it from https://github.com/freerouting/freerouting/releases/latest\n\
                 and place it at ~/.local/share/freerouting.jar"
            ))]));
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
        let dsn_path = std::env::temp_dir().join(format!("kicad_autoroute_{ts}.dsn"));
        let ses_path = std::env::temp_dir().join(format!("kicad_autoroute_{ts}.ses"));
        let dsn_str = dsn_path.to_str().unwrap_or("/tmp/route.dsn");
        let ses_str = ses_path.to_str().unwrap_or("/tmp/route.ses");

        // Step 1: export DSN via pcbnew Python API.
        // Before export, remove any duplicate-reference footprints — ExportSpecctraDSN returns
        // False (and produces no file) when the board contains two footprints with the same
        // reference designator. This can happen after a replace_footprint left a ghost.
        let export_script = format!(r#"
import pcbnew, sys
b = pcbnew.LoadBoard({path:?})
b.BuildListOfNets()
seen = set()
for fp in list(b.GetFootprints()):
    ref = fp.GetReference()
    if ref in seen:
        b.Remove(fp)
    else:
        seen.add(ref)
ok = pcbnew.ExportSpecctraDSN(b, {dsn:?})
if not ok:
    print('DSN export failed', file=sys.stderr); sys.exit(1)
print('ok')
"#,
            path = p.path, dsn = dsn_str
        );
        let export_out = Command::new("python3")
            .args(["-c", &export_script])
            .env("DISPLAY", "")
            .output().await
            .map_err(|e| McpError::internal_error(format!("python3 failed: {e}"), None))?;

        if !export_out.status.success() || !dsn_path.exists() {
            let err = String::from_utf8_lossy(&export_out.stderr);
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "DSN export failed:\n{err}"
            ))]));
        }

        // Step 2: run FreeRouting headlessly
        let mp_str = max_passes.to_string();
        let fr_out = Command::new("java")
            .args([
                "-jar", &jar,
                "--gui.enabled=false",
                "-de", dsn_str,
                "-do", ses_str,
                "-mp", &mp_str,
            ])
            .env("DISPLAY", "")
            .output().await
            .map_err(|e| McpError::internal_error(format!("java failed: {e}"), None))?;

        let fr_log = String::from_utf8_lossy(&fr_out.stderr).into_owned()
            + &String::from_utf8_lossy(&fr_out.stdout);

        if !ses_path.exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "FreeRouting did not produce a SES file.\nLog:\n{fr_log}"
            ))]));
        }

        // Step 3: import SES and save via pcbnew Python API
        let out_path = if save { p.path.clone() } else {
            std::env::temp_dir().join(format!("kicad_routed_{ts}.kicad_pcb"))
                .to_str().unwrap_or("/tmp/routed.kicad_pcb").to_string()
        };
        let import_script = format!(
            "import pcbnew\n\
             b=pcbnew.LoadBoard({path:?})\n\
             before=len(b.GetTracks())\n\
             pcbnew.ImportSpecctraSES(b,{ses:?})\n\
             after=len(b.GetTracks())\n\
             b.Save({out:?})\n\
             print(f'before={{before}} after={{after}}')",
            path = p.path, ses = ses_str, out = out_path
        );
        let import_out = Command::new("python3")
            .args(["-c", &import_script])
            .env("DISPLAY", "")
            .output().await
            .map_err(|e| McpError::internal_error(format!("python3 failed: {e}"), None))?;

        let _ = fs::remove_file(&dsn_path).await;
        let _ = fs::remove_file(&ses_path).await;

        if !import_out.status.success() {
            let err = String::from_utf8_lossy(&import_out.stderr);
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "SES import failed:\n{err}"
            ))]));
        }

        let import_info = String::from_utf8_lossy(&import_out.stdout).trim().to_string();

        // Extract routing stats from FreeRouting log
        let stats = fr_log.lines()
            .filter(|l| l.contains("completed") || l.contains("unrouted") || l.contains("score"))
            .last()
            .unwrap_or("")
            .to_string();

        // Run DRC to get structured unrouted net info
        let drc_tmp = std::env::temp_dir().join(format!("kicad_autoroute_drc_{ts}.json"));
        let unrouted_report = if let Ok((_, _, 0)) = run_kicad_cli(&[
            "pcb", "drc",
            "--output", drc_tmp.to_str().unwrap_or("/tmp/drc.json"),
            "--format", "json",
            "--severity-error",
            &out_path,
        ]).await {
            let drc_json = fs::read_to_string(&drc_tmp).await.unwrap_or_default();
            let _ = fs::remove_file(&drc_tmp).await;
            // Extract net names from unconnected_items descriptions like "PTH pad 3 [GND] of U1"
            let v: serde_json::Value = serde_json::from_str(&drc_json).unwrap_or_default();
            let unconnected = v["unconnected_items"].as_array()
                .map(|items| {
                    let mut nets: std::collections::HashSet<String> = std::collections::HashSet::new();
                    for item in items {
                        for sub in item["items"].as_array().unwrap_or(&vec![]) {
                            let desc = sub["description"].as_str().unwrap_or("");
                            // Extract net name from "[NET_NAME]" in description
                            if let (Some(a), Some(b)) = (desc.find('['), desc.find(']')) {
                                nets.insert(desc[a+1..b].to_string());
                            }
                        }
                    }
                    nets
                })
                .unwrap_or_default();
            if unconnected.is_empty() {
                "All nets routed successfully.".to_string()
            } else {
                let mut net_list: Vec<_> = unconnected.into_iter().collect();
                net_list.sort();
                format!("Unrouted nets ({}):\n  {}", net_list.len(), net_list.join(", "))
            }
        } else {
            String::new()
        };

        let summary = format!(
            "Autoroute complete: {import_info}\nRouting result: {stats}\nSaved to: {out_path}\n{unrouted_report}"
        );

        let mut contents = vec![Content::text(summary)];
        contents.extend(self.render_board(&out_path).await);
        Ok(CallToolResult::success(contents))
    }

    // ---- Schematic cleanup -------------------------------------------------

    /// Remove dangling wire segments from a schematic — wires with at least one
    /// endpoint that connects to nothing (no pin, label, junction, or other wire).
    #[tool(description = "Remove dangling wire segments from a .kicad_sch schematic — wires with an endpoint not connected to any pin, label, junction, or other wire. Run after replace_symbol to clean up disconnected wires. Returns a render.")]
    async fn cleanup_dangling_wires(
        &self,
        params: Parameters<CleanupDanglingWiresParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let (new_content, removed) = remove_dangling_wires(&content);

        let summary = if p.dry_run.unwrap_or(false) {
            format!("Dry run: would remove {removed} dangling wire segment(s).")
        } else {
            if let Err(e) = fs::write(&path, &new_content).await {
                return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
            }
            format!("Removed {removed} dangling wire segment(s).")
        };

        let mut contents = vec![Content::text(summary)];
        if !p.dry_run.unwrap_or(false) {
            if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
                contents.push(img);
            }
        }
        Ok(CallToolResult::success(contents))
    }

    // ---- kicad2print 3D substrate conversion -------------------------------

    /// Convert a KiCad PCB to a 3D-printable substrate (STL/3MF) using kicad2print.
    /// Returns a software-rendered preview of the substrate geometry.
    ///
    /// Use the optional `side` parameter to generate a model for a single side:
    /// - `side = "top"`  → exports only the top-layer model (files suffixed `_top`)
    /// - `side = "bottom"` → exports only the bottom-layer model (files suffixed `_bottom`)
    /// Omit `side` to produce the combined model containing both faces.
    #[tool(description = "Convert a .kicad_pcb file to a 3D-printable substrate model (STL/3MF). Optional 'side'='top'|'bottom' exports only that side (files suffixed _top/_bottom). Returns a preview image.")]
    async fn convert_pcb(
        &self,
        params: Parameters<ConvertPcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = PathBuf::from(&p.input_path);

        let mut config = Config::from_file(std::path::Path::new("kicad2print.toml"))
            .unwrap_or_default();
        if let Some(dir) = p.output_dir { config.output_dir = dir; }
        if let Some(v) = p.channel_width_mm { config.channel_width_mm = v; }
        if let Some(v) = p.channel_depth_mm { config.channel_depth_mm = v; }
        if let Some(v) = p.substrate_thickness_mm { config.substrate_thickness_mm = v; }
        if let Some(fmt) = p.output_format {
            config.output_format = fmt
                .parse()
                .map_err(|e| McpError::invalid_params(format!("Invalid output format: {e}"), None))?;
        }

        // Parse and scale the PCB
        let pcb = parser::parse_pcb(&input)
            .context("Failed to parse KiCad PCB file")
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let scale = autoscale::compute_scale_factor(&pcb, &config);
        let pcb = if (scale - 1.0).abs() > 0.001 { pcb.scale(scale) } else { pcb };

        // If a specific side was requested, generate a modified PcbData with the
        // opposite-layer traces removed so the model contains only the requested side.
        if let Some(side_raw) = p.side.as_deref() {
            match side_raw.to_lowercase().as_str() {
                "top" => {
                    let crate::pcb::PcbData { outline, traces_fcu, traces_bcu: _traces_bcu, arc_traces, vias, pads, footprints, cutouts } = pcb;
                    let modified = crate::pcb::PcbData {
                        outline,
                        traces_fcu,
                        traces_bcu: Vec::new(),
                        arc_traces,
                        vias,
                        pads,
                        footprints,
                        cutouts,
                    };

                    let mesh = geometry::generate_model(&modified, &config)
                        .context("Failed to generate 3D geometry")
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                    let tri_count = mesh.triangle_count();
                    let stem = format!("{}_top", input.file_stem().and_then(|s| s.to_str()).unwrap_or("board"));
                    let written = export::export(&mesh, &modified, &input, &stem, &config)
                        .context("Failed to export 3D model")
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                    let png_bytes = render::render_to_png(&mesh, 800, 600);
                    let b64 = BASE64_STANDARD.encode(&png_bytes);

                    let file_list: Vec<String> = written.iter().map(|f| f.display().to_string()).collect();
                    let summary = format!(
                        "Converted {} (top only) successfully.\nTriangles: {}\nOutput files:\n{}",
                        p.input_path, tri_count, file_list.join("\n")
                    );

                    return Ok(CallToolResult::success(vec![
                        Content::text(summary),
                        Content::image(b64, "image/png"),
                    ]));
                }
                "bottom" => {
                    let crate::pcb::PcbData { outline, traces_fcu: _traces_fcu, traces_bcu, arc_traces, vias, pads, footprints, cutouts } = pcb;
                    let modified = crate::pcb::PcbData {
                        outline,
                        traces_fcu: Vec::new(),
                        traces_bcu,
                        arc_traces,
                        vias,
                        pads,
                        footprints,
                        cutouts,
                    };

                    let mesh = geometry::generate_model(&modified, &config)
                        .context("Failed to generate 3D geometry")
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                    let tri_count = mesh.triangle_count();
                    let stem = format!("{}_bottom", input.file_stem().and_then(|s| s.to_str()).unwrap_or("board"));
                    let written = export::export(&mesh, &modified, &input, &stem, &config)
                        .context("Failed to export 3D model")
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                    let png_bytes = render::render_to_png(&mesh, 800, 600);
                    let b64 = BASE64_STANDARD.encode(&png_bytes);

                    let file_list: Vec<String> = written.iter().map(|f| f.display().to_string()).collect();
                    let summary = format!(
                        "Converted {} (bottom only) successfully.\nTriangles: {}\nOutput files:\n{}",
                        p.input_path, tri_count, file_list.join("\n")
                    );

                    return Ok(CallToolResult::success(vec![
                        Content::text(summary),
                        Content::image(b64, "image/png"),
                    ]));
                }
                other => {
                    return Err(McpError::invalid_params(
                        format!("Invalid side: {}. Use 'top' or 'bottom', or omit to export combined." , other),
                        None,
                    ));
                }
            }
        }

        // No specific side requested — generate combined model
        let mesh = geometry::generate_model(&pcb, &config)
            .context("Failed to generate 3D geometry")
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let tri_count = mesh.triangle_count();

        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("board");
        let written = export::export(&mesh, &pcb, &input, stem, &config)
            .context("Failed to export 3D model")
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let png_bytes = render::render_to_png(&mesh, 800, 600);
        let b64 = BASE64_STANDARD.encode(&png_bytes);

        let file_list: Vec<String> = written.iter().map(|f| f.display().to_string()).collect();
        let summary = format!(
            "Converted {} successfully.\nTriangles: {}\nOutput files:\n{}",
            p.input_path, tri_count, file_list.join("\n")
        );

        Ok(CallToolResult::success(vec![
            Content::text(summary),
            Content::image(b64, "image/png"),
        ]))
    }

    /// List all nets and their connected pads in a PCB file.
    #[tool(description = "List all nets in a .kicad_pcb file with their connected pads. Use this BEFORE editing to discover correct net names — never guess a net name. Returns net→pad mapping.")]
    async fn list_nets(
        &self,
        params: Parameters<ListNetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let content = match fs::read_to_string(&params.0.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let all_pads = parse_pcb_pads(&content);
        let mut net_map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for pad in &all_pads {
            net_map.entry(pad.net.clone())
                .or_default()
                .push(format!("{}/{}", pad.reference, pad.pad_num));
        }

        let mut output = format!("Nets in {}:\n\n", params.0.path);
        output.push_str(&format!("{:<45} {:>5}  {}\n", "Net name", "Pads", "Connected pads"));
        output.push_str(&"-".repeat(90));
        output.push('\n');
        for (net, pads) in &net_map {
            output.push_str(&format!("{:<45} {:>5}  {}\n",
                format!("\"{net}\""), pads.len(), pads.join(", ")));
        }
        output.push_str(&format!("\nTotal: {} nets across {} pads\n", net_map.len(), all_pads.len()));

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Query all pads within a rectangular region of a PCB.
    #[tool(description = "Return all footprint pads whose centre falls inside a rectangular region of a .kicad_pcb. Use before routing to discover what pads exist along a proposed trace path. Returns reference, pad number, net name, absolute position, and size.")]
    async fn query_pads_in_region(
        &self,
        params: Parameters<QueryPadsInRegionParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let all_pads = parse_pcb_pads(&content);
        let x_min = p.x1.min(p.x2);
        let x_max = p.x1.max(p.x2);
        let y_min = p.y1.min(p.y2);
        let y_max = p.y1.max(p.y2);

        let matching: Vec<&PcbPad> = all_pads.iter()
            .filter(|pad| pad.x >= x_min && pad.x <= x_max && pad.y >= y_min && pad.y <= y_max)
            .filter(|pad| {
                p.layer.as_ref().map_or(true, |l| {
                    pad.layers.iter().any(|pl| pl == l || pl == "*.Cu")
                })
            })
            .collect();

        if matching.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No pads in region ({}, {}) → ({}, {})", p.x1, p.y1, p.x2, p.y2
            ))]));
        }

        let mut output = format!("Pads in region ({}, {}) → ({}, {}):\n\n", p.x1, p.y1, p.x2, p.y2);
        output.push_str(&format!("{:<10} {:<6} {:<35} {:>8} {:>8} {:>6} {:>6}  {}\n",
            "Ref", "Pad", "Net", "X", "Y", "W(mm)", "H(mm)", "Type"));
        output.push_str(&"-".repeat(95));
        output.push('\n');
        for pad in &matching {
            output.push_str(&format!("{:<10} {:<6} {:<35} {:>8.3} {:>8.3} {:>6.3} {:>6.3}  {}\n",
                pad.reference, pad.pad_num, pad.net,
                pad.x, pad.y, pad.width, pad.height,
                if pad.is_thru_hole { "THT" } else { "SMD" }));
        }
        output.push_str(&format!("\nTotal: {} pads\n", matching.len()));

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Query all trace segments whose bounding box overlaps a rectangular region.
    #[tool(description = "Return all copper trace segments whose bounding box overlaps a rectangular region of a .kicad_pcb. Use before routing to discover what traces already occupy a proposed path — pairs with query_pads_in_region. Returns net, layer, endpoints, and width. Uses bounding-box overlap, which is conservative: a segment may occasionally be reported when only its bounding box (not the segment itself) clips the region.")]
    async fn query_traces_in_region(
        &self,
        params: Parameters<QueryTracesInRegionParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let all_segments = parse_pcb_segments(&content);
        let x_min = p.x1.min(p.x2);
        let x_max = p.x1.max(p.x2);
        let y_min = p.y1.min(p.y2);
        let y_max = p.y1.max(p.y2);

        let matching: Vec<&PcbSegment> = all_segments.iter()
            .filter(|s| segment_bbox_overlaps_region(s, x_min, y_min, x_max, y_max))
            .filter(|s| p.layer.as_deref().map_or(true, |l| s.layer == l))
            .filter(|s| p.net.as_deref().map_or(true, |n| s.net == n))
            .collect();

        if matching.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No traces in region ({}, {}) → ({}, {})", p.x1, p.y1, p.x2, p.y2
            ))]));
        }

        let mut output = format!("Traces in region ({}, {}) → ({}, {}):\n\n", p.x1, p.y1, p.x2, p.y2);
        output.push_str(&format!("{:<30} {:<6} {:>8} {:>8}    {:>8} {:>8}  {:>6}\n",
            "Net", "Layer", "X1", "Y1", "X2", "Y2", "W(mm)"));
        output.push_str(&"-".repeat(90));
        output.push('\n');
        for seg in &matching {
            let net = if seg.net.is_empty() { "<unrouted>" } else { &seg.net };
            output.push_str(&format!("{:<30} {:<6} {:>8.3} {:>8.3}    {:>8.3} {:>8.3}  {:>6.3}\n",
                net, seg.layer, seg.x1, seg.y1, seg.x2, seg.y2, seg.width));
        }
        output.push_str(&format!("\nTotal: {} traces\n", matching.len()));

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Check if a proposed trace segment collides with or violates clearance from any
    /// pad OR any existing same-layer trace on a different net.
    #[tool(description = "Before adding a trace, check if it would collide with or come too close to any pad OR any existing same-layer trace of a different net in a .kicad_pcb. Pass net to exempt same-net T-junctions. Returns collisions (physical overlap) and warnings (closer than clearance). Run this before add_trace to catch routing errors without a DRC cycle.")]
    async fn check_trace_clearance(
        &self,
        params: Parameters<CheckTraceClearanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let width = p.width.unwrap_or(0.25);
        let clearance = p.clearance.unwrap_or(0.1);
        let net = p.net.as_deref().unwrap_or("");

        let (collisions, warnings) = compute_clearance(
            &content, p.x1, p.y1, p.x2, p.y2, &p.layer, width, clearance, net,
        );

        let status = if collisions.is_empty() && warnings.is_empty() {
            "CLEAR".to_string()
        } else if !collisions.is_empty() {
            format!("{} COLLISION(S)", collisions.len())
        } else {
            format!("{} WARNING(S)", warnings.len())
        };

        let mut output = format!(
            "Clearance check [{status}]: ({}, {})→({}, {}) on {} w={:.3}mm gap={:.3}mm\n\n",
            p.x1, p.y1, p.x2, p.y2, p.layer, width, clearance
        );

        if collisions.is_empty() && warnings.is_empty() {
            output.push_str("No pads or existing traces within collision or clearance distance. Safe to route.\n");
        }
        if !collisions.is_empty() {
            output.push_str(&format!("COLLISIONS ({}) — trace physically overlaps these pads/existing traces:\n", collisions.len()));
            for c in &collisions { output.push_str(c); output.push('\n'); }
        }
        if !warnings.is_empty() {
            output.push_str(&format!("\nCLEARANCE WARNINGS ({}) — closer than {:.3}mm gap:\n", warnings.len(), clearance));
            for w in &warnings { output.push_str(w); output.push('\n'); }
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Return the net name and position for a specific pad on a footprint.
    #[tool(description = "Return the net name, absolute position, and size of a specific pad in a .kicad_pcb footprint. Use this to discover correct net names before routing or renaming nets.")]
    async fn get_net_for_pad(
        &self,
        params: Parameters<GetNetForPadParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let fp_blocks = pcb_edit::find_footprint_blocks(&content);
        let block_range = match fp_blocks.get(&p.reference) {
            Some(r) => r.clone(),
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Footprint '{}' not found. Available refs: {}",
                p.reference,
                fp_blocks.keys().cloned().collect::<Vec<_>>().join(", ")
            ))])),
        };
        let block = &content[block_range];

        let (fp_x, fp_y, fp_rot) = pcb_edit::extract_at(block).unwrap_or((0.0, 0.0, 0.0));
        let rot_rad = fp_rot.to_radians();

        let pad_marker = format!("(pad \"{}\"", p.pad_number);
        let rel = match block.find(&pad_marker) {
            Some(r) => r,
            None => {
                // Collect available pad numbers for error message
                let mut avail = Vec::new();
                let mut s = 0;
                while let Some(r) = block[s..].find("(pad \"") {
                    let end = pcb_edit::block_end(block, s + r);
                    if let Some(n) = extract_pad_number(&block[s + r..end]) {
                        avail.push(n);
                    }
                    s = end;
                }
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Pad '{}' not found in '{}'. Available pads: {}",
                    p.pad_number, p.reference, avail.join(", ")
                ))]));
            }
        };

        let pad_end = pcb_edit::block_end(block, rel);
        let pad_block = &block[rel..pad_end];

        let net = extract_pcb_pad_net(pad_block).unwrap_or_else(|| "(unconnected)".to_string());
        let (dx, dy) = extract_pad_at(pad_block).unwrap_or((0.0, 0.0));
        let (pw, ph) = extract_pad_size(pad_block).unwrap_or((0.0, 0.0));
        let pad_type = if pad_block.contains("thru_hole") { "thru_hole" } else { "smd" };

        let abs_x = fp_x + dx * rot_rad.cos() - dy * rot_rad.sin();
        let abs_y = fp_y + dx * rot_rad.sin() + dy * rot_rad.cos();

        let output = format!(
            "Pad {}/{}: net=\"{}\"  pos=({:.3}, {:.3})mm  size={:.3}×{:.3}mm  type={}  (fp at ({:.3},{:.3}) rot={:.1}°)",
            p.reference, p.pad_number, net, abs_x, abs_y, pw, ph, pad_type, fp_x, fp_y, fp_rot
        );

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Check if two pads are electrically connected by existing traces/vias.
    #[tool(description = "Check whether two pads in a .kicad_pcb are electrically connected by existing traces and vias (i.e. would pass a connectivity/ratsnest check). Returns CONNECTED or DISCONNECTED with a path summary. Use after adding traces to confirm routing completeness before running full DRC.")]
    async fn verify_connectivity(
        &self,
        params: Parameters<VerifyConnectivityParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let content = match fs::read_to_string(&p.path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        let pads = parse_pcb_pads(&content);
        let segments = parse_pcb_segments(&content);
        let vias = parse_pcb_vias(&content);

        let result = check_pad_connectivity(&pads, &segments, &vias,
            &p.ref_a, &p.pad_a, &p.ref_b, &p.pad_b);

        let output = match result {
            Ok(true) => format!(
                "CONNECTED: {}/{} ↔ {}/{} are electrically connected by existing traces/vias.\n\
                 (Segments checked: {}, Vias checked: {})",
                p.ref_a, p.pad_a, p.ref_b, p.pad_b, segments.len(), vias.len()
            ),
            Ok(false) => format!(
                "DISCONNECTED: {}/{} ↔ {}/{} have no trace path between them.\n\
                 A ratsnest line exists — routing is incomplete.\n\
                 (Segments checked: {}, Vias checked: {})",
                p.ref_a, p.pad_a, p.ref_b, p.pad_b, segments.len(), vias.len()
            ),
            Err(e) => format!("Error: {e}"),
        };

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Add a power symbol (net label) to a KiCad schematic, including its lib_symbols definition.
    #[tool(description = "Add a power net symbol (e.g. VBUS, GND, +5V) to a .kicad_sch schematic. Looks up the symbol definition in the installed KiCad power library, embeds it in lib_symbols if not already present, and places an instance at the given position. Renders a schematic preview. Use this instead of manually editing the file to add power connections.")]
    async fn add_power_symbol(
        &self,
        params: Parameters<AddPowerSymbolParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let path = PathBuf::from(&p.path);
        let _guard = self.lock_file(&path).await;

        let mut content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to read: {e}"))])),
        };

        // 1. Find and read the power symbol library
        let lib_path = match find_power_symbol_lib() {
            Some(p) => p,
            None => return Ok(CallToolResult::error(vec![Content::text(
                "KiCad power symbol library not found. Install kicad-symbols or check your KiCad installation.".to_string()
            )])),
        };
        let lib_content = match std::fs::read_to_string(&lib_path) {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read power library {}: {e}", lib_path.display()
            ))])),
        };

        // 2. Extract the symbol definition block
        let sym_def = match extract_lib_symbol(&lib_content, &p.net_name) {
            Some(d) => d,
            None => return Ok(CallToolResult::error(vec![Content::text(format!(
                "Symbol 'power:{}' not found in {}.\n\
                 Check the net name — it must exactly match a symbol in the KiCad power library.\n\
                 Common names: GND, VBUS, +5V, +3.3V, VCC, PWR_FLAG",
                p.net_name, lib_path.display()
            ))])),
        };

        // 3. Check if lib_symbols already contains this symbol
        let lib_id = format!("power:{}", p.net_name);
        let already_in_lib = content.contains(&format!("(symbol \"{lib_id}\""));

        if !already_in_lib {
            // Insert the definition inside (lib_symbols ...)
            let marker = "(lib_symbols";
            if let Some(pos) = content.find(marker) {
                // Find the opening paren and skip to just inside it
                let insert_after = content[pos..].find('\n').map(|r| pos + r + 1).unwrap_or(pos + marker.len());
                content.insert_str(insert_after, &format!("{sym_def}\n"));
            } else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No (lib_symbols ...) section found in schematic. Is this a valid .kicad_sch file?".to_string()
                )]));
            }
        }

        // 4. Extract project name and root path UUID for the instance
        let project_name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();

        let root_uuid = {
            // Find an existing (path "/UUID" ...) from any instance in the schematic
            let mut found = String::from("00000000-0000-0000-0000-000000000000");
            if let Some(pos) = content.find("(path \"/") {
                let after = &content[pos + 8..];
                if let Some(end) = after.find('"') {
                    found = after[..end].to_string();
                }
            }
            found
        };

        // 5. Find next available #PWR reference number
        let mut max_pwr = 0u32;
        let mut search_pwr = 0;
        while let Some(rel) = content[search_pwr..].find("#PWR") {
            let start = search_pwr + rel + 4;
            let digits: String = content[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                max_pwr = max_pwr.max(n);
            }
            search_pwr = start;
        }
        let pwr_ref = format!("#PWR{:03}", max_pwr + 1);

        // 6. Generate UUIDs for the new instance
        let inst_uuid = pcb_edit::new_tstamp();
        let pin_uuid  = pcb_edit::new_tstamp();
        let rotation  = p.rotation.unwrap_or(0.0);

        // 7. Build the placed symbol instance S-expression
        let desc_line = format!("Power symbol creates a global label with name \"{net}\"", net = p.net_name);
        let instance_sexpr = format!(
            "\n\t(symbol\n\
             \t\t(lib_id \"{lib_id}\")\n\
             \t\t(at {x} {y} {rot})\n\
             \t\t(unit 1)\n\
             \t\t(body_style 1)\n\
             \t\t(exclude_from_sim no)\n\
             \t\t(in_bom yes)\n\
             \t\t(on_board yes)\n\
             \t\t(in_pos_files yes)\n\
             \t\t(dnp no)\n\
             \t\t(fields_autoplaced yes)\n\
             \t\t(uuid \"{inst_uuid}\")\n\
             \t\t(property \"Reference\" \"{pwr_ref}\"\n\
             \t\t\t(at {x} {py_ref} 0)\n\
             \t\t\t(hide yes)\n\
             \t\t\t(show_name no)\n\
             \t\t\t(do_not_autoplace no)\n\
             \t\t\t(effects (font (size 1.27 1.27)))\n\
             \t\t)\n\
             \t\t(property \"Value\" \"{net}\"\n\
             \t\t\t(at {x} {py_val} 0)\n\
             \t\t\t(show_name no)\n\
             \t\t\t(do_not_autoplace no)\n\
             \t\t\t(effects (font (size 1.27 1.27)))\n\
             \t\t)\n\
             \t\t(property \"Footprint\" \"\"\n\
             \t\t\t(at {x} {y} 0)\n\
             \t\t\t(hide yes)\n\
             \t\t\t(show_name no)\n\
             \t\t\t(do_not_autoplace no)\n\
             \t\t\t(effects (font (size 1.27 1.27)))\n\
             \t\t)\n\
             \t\t(property \"Datasheet\" \"\"\n\
             \t\t\t(at {x} {y} 0)\n\
             \t\t\t(hide yes)\n\
             \t\t\t(show_name no)\n\
             \t\t\t(do_not_autoplace no)\n\
             \t\t\t(effects (font (size 1.27 1.27)))\n\
             \t\t)\n\
             \t\t(property \"Description\" \"{desc}\"\n\
             \t\t\t(at {x} {y} 0)\n\
             \t\t\t(hide yes)\n\
             \t\t\t(show_name no)\n\
             \t\t\t(do_not_autoplace no)\n\
             \t\t\t(effects (font (size 1.27 1.27)))\n\
             \t\t)\n\
             \t\t(pin \"1\" (uuid \"{pin_uuid}\"))\n\
             \t\t(instances\n\
             \t\t\t(project \"{proj}\"\n\
             \t\t\t\t(path \"/{root}\"\n\
             \t\t\t\t\t(reference \"{pwr_ref}\")\n\
             \t\t\t\t\t(unit 1)\n\
             \t\t\t\t)\n\
             \t\t\t)\n\
             \t\t)\n\
             \t)",
            lib_id = lib_id,
            x = p.x, y = p.y, rot = rotation,
            py_ref = p.y + 3.81, py_val = p.y - 3.556,
            net = p.net_name,
            pwr_ref = pwr_ref,
            desc = desc_line,
            inst_uuid = inst_uuid, pin_uuid = pin_uuid,
            proj = project_name, root = root_uuid,
        );

        // Insert before final closing paren
        if let Some(pos) = content.rfind("\n)") {
            content.insert_str(pos, &instance_sexpr);
        } else {
            return Ok(CallToolResult::error(vec![Content::text(
                "Could not find end of schematic file.".to_string()
            )]));
        }

        if let Err(e) = fs::write(&path, &content).await {
            return Ok(CallToolResult::error(vec![Content::text(format!("Failed to write: {e}"))]));
        }

        let lib_note = if already_in_lib { " (lib_symbols already present)" } else { " (lib_symbols definition added)" };
        let mut result = vec![Content::text(format!(
            "Added power symbol 'power:{}' at ({}, {}) rot={}° as {}{}\nLib: {}",
            p.net_name, p.x, p.y, rotation, pwr_ref, lib_note, lib_path.display()
        ))];

        if let Some(img) = self.render_schematic_png(&p.path, None, false, 2400).await {
            result.push(img);
        }
        Ok(CallToolResult::success(result))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for KiCadServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new("kicad2print", "0.1.0");
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Recursively collect KiCad files up to `max_depth` levels deep.
async fn collect_kicad_files(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    pcb: &mut Vec<PathBuf>,
    sch: &mut Vec<PathBuf>,
    pro: &mut Vec<PathBuf>,
) {
    let Ok(mut rd) = fs::read_dir(dir).await else { return };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if p.is_dir() && depth < max_depth {
            // Don't descend into hidden dirs or build dirs
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" && name != "output" {
                Box::pin(collect_kicad_files(&p, depth + 1, max_depth, pcb, sch, pro)).await;
            }
        } else {
            match p.extension().and_then(|e| e.to_str()) {
                Some("kicad_pcb") => pcb.push(p),
                Some("kicad_sch") => sch.push(p),
                Some("kicad_pro") => pro.push(p),
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schematic symbol editing helpers
// ---------------------------------------------------------------------------

/// Returns byte ranges of every symbol instance block in a schematic file, keyed by reference.
/// Only matches top-level symbol blocks (2-space indent): `\n  (symbol (lib_id "`.
fn find_symbol_blocks(content: &str) -> std::collections::HashMap<String, std::ops::Range<usize>> {
    let mut result = std::collections::HashMap::new();
    // Matches both the compact single-line form `(symbol (lib_id "...")` and the
    // modern multi-line form where `(symbol` and `(lib_id ...)` sit on separate
    // lines, and both 2-space and tab indentation — for_each_top_level only checks
    // the leading whitespace on the matched line, not what follows the keyword.
    // This also correctly skips nested `(symbol "Library:Name" ...)` library
    // definitions inside `(lib_symbols ...)`, which sit two indent levels deep.
    for_each_top_level(content, "(symbol", |start, end| {
        let block = &content[start..end];
        if let Some(reference) = sch_property_value(block, "Reference") {
            result.insert(reference, start..end);
        }
    });
    result
}

/// Extract the value of `(property "KEY" "VALUE" ...)` from a schematic symbol block.
fn sch_property_value(block: &str, key: &str) -> Option<String> {
    let marker = format!("(property \"{}\" \"", key);
    let pos = block.find(&marker)?;
    let after = &block[pos + marker.len()..];
    // Value may contain escaped quotes; scan char by char
    let mut value = String::new();
    let mut chars = after.chars();
    loop {
        match chars.next()? {
            '\\' => { chars.next(); } // skip escaped char
            '"' => break,
            c => value.push(c),
        }
    }
    Some(value)
}

/// Build a map of pin_number → pin_name from the lib_symbols section of the schematic,
/// for the symbol with the given lib_id (e.g. "Library:SymbolName").
fn extract_symbol_pins(lib_symbols_block: &str, lib_id: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    // Find the symbol definition block inside lib_symbols
    let marker = format!("(symbol \"{}\"", lib_id);
    let start = match lib_symbols_block.find(&marker) {
        Some(s) => s,
        None => return map,
    };
    let end = pcb_edit::block_end(lib_symbols_block, start);
    let sym_block = &lib_symbols_block[start..end];

    // Walk through all (pin ...) entries inside nested (symbol "..._N_M" ...) sub-blocks
    // Pin format: (pin TYPE STYLE (at ...) (length ...) (name "NAME" ...) (number "NUM" ...) )
    let mut search = 0;
    while let Some(rel) = sym_block[search..].find("\n        (pin ") {
        let pin_start = search + rel + 1; // skip '\n'
        let pin_end = pcb_edit::block_end(sym_block, pin_start);
        let pin_block = &sym_block[pin_start..pin_end];

        let name = extract_pin_field(pin_block, "name");
        let number = extract_pin_field(pin_block, "number");

        if let (Some(n), Some(num)) = (name, number) {
            map.insert(num, n);
        }
        search = pin_end;
    }
    map
}

/// Extract `(name "VALUE" ...)` or `(number "VALUE" ...)` from a pin block.
fn extract_pin_field(pin_block: &str, field: &str) -> Option<String> {
    let marker = format!("({} \"", field);
    let pos = pin_block.find(&marker)?;
    let after = &pin_block[pos + marker.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Find and extract the symbol definition block from lib_symbols for the given lib_id.
/// Returns (start, end) byte offsets within `lib_symbols_block`, or None if not found.
fn find_lib_symbol_def(lib_symbols_block: &str, lib_id: &str) -> Option<std::ops::Range<usize>> {
    let marker = format!("(symbol \"{}\"", lib_id);
    let start = lib_symbols_block.find(&marker)?;
    let end = pcb_edit::block_end(lib_symbols_block, start);
    Some(start..end)
}

/// Given the old and new pin number→name maps and the old pin UUID list,
/// compute the new pin list: [(pin_number, uuid)] where UUIDs are preserved
/// when the pin name matches, fresh otherwise.
fn remap_pin_uuids(
    old_pins_by_num: &std::collections::HashMap<String, String>, // num → name (old sym def)
    new_pins_by_num: &std::collections::HashMap<String, String>, // num → name (new sym def)
    old_instance_pins: &[(String, String)],                       // (num, uuid) from old instance
) -> (Vec<(String, String)>, Vec<String>) {
    // Build: name → uuid for old instance pins
    let mut old_name_to_uuid: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (num, uuid) in old_instance_pins {
        if let Some(name) = old_pins_by_num.get(num) {
            old_name_to_uuid.insert(name.clone(), uuid.clone());
        }
    }

    let mut new_pins: Vec<(String, String)> = Vec::new();
    let mut dangling: Vec<String> = Vec::new();

    // For each pin in the new symbol definition, find a matching old UUID by name
    let mut new_nums: Vec<&String> = new_pins_by_num.keys().collect();
    new_nums.sort_by(|a, b| {
        // Sort numerically if possible, lexicographically otherwise
        match (a.parse::<i64>(), b.parse::<i64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => a.cmp(b),
        }
    });

    for num in new_nums {
        let name = &new_pins_by_num[num];
        let uuid = if let Some(old_uuid) = old_name_to_uuid.get(name) {
            old_uuid.clone()
        } else {
            pcb_edit::new_tstamp()
        };
        new_pins.push((num.clone(), uuid));
    }

    // Report pins that existed in old symbol but have no match in new symbol
    let new_names: std::collections::HashSet<&String> = new_pins_by_num.values().collect();
    for old_name in old_pins_by_num.values() {
        if !new_names.contains(old_name) {
            dangling.push(old_name.clone());
        }
    }

    (new_pins, dangling)
}

/// Rebuild a schematic symbol instance block with new lib_id, value, footprint and pins.
fn rebuild_symbol_instance(
    old_block: &str,
    new_lib_id: &str,
    new_value: &str,
    new_footprint: &str,
    new_pins: &[(String, String)],
) -> String {
    let mut result = old_block.to_string();

    // Replace lib_id
    {
        let prefix = "(symbol (lib_id \"";
        if let Some(pos) = result.find(prefix) {
            let name_start = pos + prefix.len();
            if let Some(end_rel) = result[name_start..].find('"') {
                result = format!("{}{}{}", &result[..name_start], new_lib_id, &result[name_start + end_rel..]);
            }
        }
    }

    // Replace Value property
    result = replace_sch_property(&result, "Value", new_value);

    // Replace Footprint property
    result = replace_sch_property(&result, "Footprint", new_footprint);

    // Rebuild pin list — find first pin line and replace everything from there to end-1
    // The last character of old_block is ')' at the end
    let pin_section_start = result.find("\n    (pin \"");
    let block_close = result.rfind("\n  )").unwrap_or(result.len() - 3);

    let mut pins_text = String::new();
    for (num, uuid) in new_pins {
        pins_text.push_str(&format!("\n    (pin \"{}\" (uuid \"{}\"))", num, uuid));
    }
    pins_text.push_str("\n  )");

    if let Some(ps) = pin_section_start {
        result = format!("{}{}", &result[..ps], pins_text);
    } else {
        // No existing pins — replace the closing paren
        result = format!("{}{}", &result[..block_close], pins_text);
    }

    result
}

/// Replace `(property "KEY" "VALUE" ...)` value in a schematic symbol block.
fn replace_sch_property(block: &str, key: &str, new_value: &str) -> String {
    let marker = format!("(property \"{}\" \"", key);
    if let Some(pos) = block.find(&marker) {
        let val_start = pos + marker.len();
        if let Some(end_rel) = block[val_start..].find('"') {
            return format!("{}{}{}", &block[..val_start], new_value, &block[val_start + end_rel..]);
        }
    }
    block.to_string()
}

/// Extract all pin entries from a schematic symbol instance block.
/// Returns Vec<(pin_number, uuid)>.
fn extract_instance_pins(block: &str) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    let mut search = 0;
    while let Some(rel) = block[search..].find("\n    (pin \"") {
        let line_start = search + rel + 1;
        // Find closing paren for this pin line
        let line_end = block[line_start..].find('\n').map(|e| line_start + e).unwrap_or(block.len());
        let line = &block[line_start..line_end];
        // (pin "NUM" (uuid "UUID"))
        if let Some(num) = extract_quoted_after(line, "(pin \"") {
            if let Some(uuid) = extract_quoted_after(line, "(uuid \"") {
                pins.push((num, uuid));
            }
        }
        search = line_end;
    }
    pins
}

/// Extract the first quoted string after `prefix` within `text`.
fn extract_quoted_after(text: &str, prefix: &str) -> Option<String> {
    let pos = text.find(prefix)?;
    let after = &text[pos + prefix.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

// ---------------------------------------------------------------------------
// PCB spatial parsing helpers (used by list_nets, query_pads_in_region, etc.)
// ---------------------------------------------------------------------------

/// One pad from a PCB footprint, with computed absolute position.
struct PcbPad {
    reference: String,
    pad_num: String,
    net: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    is_thru_hole: bool,
    layers: Vec<String>,
}

/// Extract the pad number from a pad block: `(pad "NUM" ...`.
fn extract_pad_number(block: &str) -> Option<String> {
    let after = block.strip_prefix("(pad \"")?;
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Extract `(at DX DY)` from a pad block (relative position inside footprint).
fn extract_pad_at(block: &str) -> Option<(f64, f64)> {
    let pos = block.find("(at ")?;
    let after = &block[pos + 4..];
    let close = after.find(')')?;
    let args: Vec<f64> = after[..close]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match args.as_slice() {
        [x, y, ..] => Some((*x, *y)),
        _ => None,
    }
}

/// Extract `(size W H)` from a pad block.
fn extract_pad_size(block: &str) -> Option<(f64, f64)> {
    let pos = block.find("(size ")?;
    let after = &block[pos + 6..];
    let close = after.find(')')?;
    let args: Vec<f64> = after[..close]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match args.as_slice() {
        [w, h] => Some((*w, *h)),
        _ => None,
    }
}

/// Extract net name from `(net "NETNAME")` in a pad block.
fn extract_pcb_pad_net(block: &str) -> Option<String> {
    let prefix = "(net \"";
    let pos = block.find(prefix)?;
    let after = &block[pos + prefix.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Parse all pads from a PCB file with computed absolute positions.
fn parse_pcb_pads(content: &str) -> Vec<PcbPad> {
    use pcb_edit::{block_end, extract_at, find_footprint_blocks};

    let mut result = Vec::new();
    let fp_blocks = find_footprint_blocks(content);

    for (reference, range) in &fp_blocks {
        let block = &content[range.clone()];
        let (fp_x, fp_y, fp_rot) = extract_at(block).unwrap_or((0.0, 0.0, 0.0));
        let rot_rad = fp_rot.to_radians();
        let cos_r = rot_rad.cos();
        let sin_r = rot_rad.sin();

        let mut search = 0;
        while let Some(rel) = block[search..].find("(pad \"") {
            let pad_start = search + rel;
            let pad_end = block_end(block, pad_start);
            let pad_block = &block[pad_start..pad_end];

            if let Some(pad_num) = extract_pad_number(pad_block) {
                let (dx, dy) = extract_pad_at(pad_block).unwrap_or((0.0, 0.0));
                let (pw, ph) = extract_pad_size(pad_block).unwrap_or((0.0, 0.0));
                let net = extract_pcb_pad_net(pad_block).unwrap_or_default();
                let is_thru_hole = pad_block.contains("thru_hole") || pad_block.contains("\"*.Cu\"");
                let layers = parse_layers_field(pad_block);

                // Apply footprint rotation to pad local offset
                let abs_x = fp_x + dx * cos_r - dy * sin_r;
                let abs_y = fp_y + dx * sin_r + dy * cos_r;

                result.push(PcbPad {
                    reference: reference.clone(),
                    pad_num,
                    net,
                    x: abs_x,
                    y: abs_y,
                    width: pw,
                    height: ph,
                    is_thru_hole,
                    layers,
                });
            }
            search = pad_end;
        }
    }
    result
}

/// Minimum distance from point (px, py) to the segment A→B.
fn point_to_segment_dist(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let t = ((px - ax) * dx + (py - ay) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// Minimum distance between segment A1→A2 and segment B1→B2. Returns 0.0 if
/// the two segments properly intersect (or touch); otherwise the minimum of
/// the four endpoint-to-opposite-segment distances.
fn segment_to_segment_dist(
    ax1: f64, ay1: f64, ax2: f64, ay2: f64,
    bx1: f64, by1: f64, bx2: f64, by2: f64,
) -> f64 {
    // Standard orientation test: sign of the cross product (p2-p1) x (p3-p1).
    fn orientation(px: f64, py: f64, qx: f64, qy: f64, rx: f64, ry: f64) -> f64 {
        (qx - px) * (ry - py) - (qy - py) * (rx - px)
    }
    fn on_segment(px: f64, py: f64, qx: f64, qy: f64, rx: f64, ry: f64) -> bool {
        // Assumes p, q, r are collinear; checks r lies within the p..q bounding box.
        rx <= px.max(qx) + 1e-9 && rx >= px.min(qx) - 1e-9
            && ry <= py.max(qy) + 1e-9 && ry >= py.min(qy) - 1e-9
    }

    let o1 = orientation(ax1, ay1, ax2, ay2, bx1, by1);
    let o2 = orientation(ax1, ay1, ax2, ay2, bx2, by2);
    let o3 = orientation(bx1, by1, bx2, by2, ax1, ay1);
    let o4 = orientation(bx1, by1, bx2, by2, ax2, ay2);

    let general_case = (o1 > 0.0) != (o2 > 0.0) && (o3 > 0.0) != (o4 > 0.0);
    let collinear_overlap = (o1.abs() < 1e-9 && on_segment(ax1, ay1, ax2, ay2, bx1, by1))
        || (o2.abs() < 1e-9 && on_segment(ax1, ay1, ax2, ay2, bx2, by2))
        || (o3.abs() < 1e-9 && on_segment(bx1, by1, bx2, by2, ax1, ay1))
        || (o4.abs() < 1e-9 && on_segment(bx1, by1, bx2, by2, ax2, ay2));

    if general_case || collinear_overlap {
        return 0.0;
    }

    point_to_segment_dist(ax1, ay1, bx1, by1, bx2, by2)
        .min(point_to_segment_dist(ax2, ay2, bx1, by1, bx2, by2))
        .min(point_to_segment_dist(bx1, by1, ax1, ay1, ax2, ay2))
        .min(point_to_segment_dist(bx2, by2, ax1, ay1, ax2, ay2))
}

/// Shared clearance-check core used by both the `check_trace_clearance` tool
/// and `add_trace`'s optional pre-write check. Checks a proposed segment
/// against every pad (any layer, conservative) and every existing same-layer
/// trace on a DIFFERENT net (same-net copper is allowed to touch/overlap at
/// T-junctions and vias). Returns (collisions, warnings) as formatted lines.
fn compute_clearance(
    content: &str,
    x1: f64, y1: f64, x2: f64, y2: f64,
    layer: &str, width: f64, clearance: f64, net: &str,
) -> (Vec<String>, Vec<String>) {
    let half_trace = width / 2.0;
    let mut collisions: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let all_pads = parse_pcb_pads(content);
    for pad in &all_pads {
        // THT pads affect all layers; SMD pads we check regardless (conservative)
        let dist = point_to_segment_dist(pad.x, pad.y, x1, y1, x2, y2);
        let half_pad = (pad.width.max(pad.height)) / 2.0;
        let collision_dist = half_trace + half_pad;
        let warn_dist = collision_dist + clearance;

        if dist < collision_dist {
            collisions.push(format!(
                "  COLLISION  {:<10} pad {:>4}  net={:<30}  pos=({:.3},{:.3})  size={:.3}×{:.3}  dist={:.3}mm",
                pad.reference, pad.pad_num, pad.net, pad.x, pad.y, pad.width, pad.height, dist
            ));
        } else if dist < warn_dist {
            warnings.push(format!(
                "  CLOSE      {:<10} pad {:>4}  net={:<30}  pos=({:.3},{:.3})  size={:.3}×{:.3}  dist={:.3}mm  (min={:.3}mm)",
                pad.reference, pad.pad_num, pad.net, pad.x, pad.y, pad.width, pad.height, dist, warn_dist
            ));
        }
    }

    // Trace-vs-trace: the actual gap this check used to miss entirely — a
    // proposed segment could silently cross an existing same-layer, different-net
    // trace with no warning at all until a much later run_drc call.
    let all_segments = parse_pcb_segments(content);
    for seg in &all_segments {
        if seg.layer != layer { continue; }
        // Same-net copper touching is a legitimate T-junction/continuation, not a
        // collision — but an EMPTY proposed net is never treated as a wildcard
        // match against other empty/unrouted segments, since two unrouted traces
        // touching is exactly the failure mode this check exists to catch.
        if !net.is_empty() && seg.net == net { continue; }

        let dist = segment_to_segment_dist(x1, y1, x2, y2, seg.x1, seg.y1, seg.x2, seg.y2);
        let half_other = seg.width / 2.0;
        let collision_dist = half_trace + half_other;
        let warn_dist = collision_dist + clearance;
        let seg_net = if seg.net.is_empty() { "<unrouted>" } else { &seg.net };

        if dist < collision_dist {
            collisions.push(format!(
                "  COLLISION  trace      net={:<30}  layer={:<6}  ({:.3},{:.3})→({:.3},{:.3})  width={:.3}mm  dist={:.3}mm",
                seg_net, seg.layer, seg.x1, seg.y1, seg.x2, seg.y2, seg.width, dist
            ));
        } else if dist < warn_dist {
            warnings.push(format!(
                "  CLOSE      trace      net={:<30}  layer={:<6}  ({:.3},{:.3})→({:.3},{:.3})  width={:.3}mm  dist={:.3}mm  (min={:.3}mm)",
                seg_net, seg.layer, seg.x1, seg.y1, seg.x2, seg.y2, seg.width, dist, warn_dist
            ));
        }
    }

    (collisions, warnings)
}

/// One routed segment parsed from a PCB file.
struct PcbSegment {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    layer: String,
    net: String,
    width: f64,
}

/// Parse all `(segment ...)` entries from a PCB file.
fn parse_pcb_segments(content: &str) -> Vec<PcbSegment> {
    let mut result = Vec::new();
    // for_each_top_level handles both 2-space (KiCad 6/7) and tab (KiCad 9/10)
    // indentation — the previous hardcoded "\n\t(segment" search only matched
    // tab-indented files, silently finding zero segments in 2-space files.
    for_each_top_level(content, "(segment", |seg_start, seg_end| {
        let block = &content[seg_start..seg_end];

        // Parse (start X Y) and (end X Y)
        let start = parse_xy_field(block, "(start ");
        let end   = parse_xy_field(block, "(end ");
        let layer = parse_quoted_field(block, "(layer \"");
        let net = parse_quoted_field(block, "(net \"").unwrap_or_default();
        let width = parse_number_field(block, "(width ").unwrap_or(0.25);

        if let (Some((x1, y1)), Some((x2, y2)), Some(layer)) = (start, end, layer) {
            result.push(PcbSegment { x1, y1, x2, y2, layer, net, width });
        }
    });
    result
}

/// Parse a single numeric `(KEYWORD N)` field, e.g. `(width 0.4)`.
fn parse_number_field(block: &str, keyword: &str) -> Option<f64> {
    let pos = block.find(keyword)?;
    let after = &block[pos + keyword.len()..];
    let close = after.find(')')?;
    after[..close].trim().parse().ok()
}

/// AABB-vs-AABB overlap test between a segment's own bounding box and a query
/// region. Conservative (a shallow diagonal segment could rarely be reported
/// when only its bbox — not the segment itself — clips the region), same spirit
/// as `check_trace_clearance`'s existing layer approximation.
fn segment_bbox_overlaps_region(seg: &PcbSegment, x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> bool {
    let (sxmin, sxmax) = (seg.x1.min(seg.x2), seg.x1.max(seg.x2));
    let (symin, symax) = (seg.y1.min(seg.y2), seg.y1.max(seg.y2));
    sxmin <= x_max && sxmax >= x_min && symin <= y_max && symax >= y_min
}

/// One via parsed from a PCB file.
struct PcbVia {
    x: f64,
    y: f64,
    layers: Vec<String>,
}

/// Parse all `(via ...)` entries from a PCB file.
fn parse_pcb_vias(content: &str) -> Vec<PcbVia> {
    use pcb_edit::block_end;
    let mut result = Vec::new();
    let mut search = 0;
    while let Some(rel) = content[search..].find("\n\t(via") {
        let via_start = search + rel + 1;
        let via_end = block_end(content, via_start);
        let block = &content[via_start..via_end];

        if let Some((x, y)) = parse_xy_field(block, "(at ") {
            let layers = parse_layers_field(block);
            result.push(PcbVia { x, y, layers });
        }
        search = via_end;
    }
    result
}

/// Parse `(KEYWORD X Y)` returning (x, y).
fn parse_xy_field(block: &str, keyword: &str) -> Option<(f64, f64)> {
    let pos = block.find(keyword)?;
    let after = &block[pos + keyword.len()..];
    let close = after.find(')')?;
    let args: Vec<f64> = after[..close]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match args.as_slice() {
        [x, y, ..] => Some((*x, *y)),
        _ => None,
    }
}

/// Parse `(KEYWORD "VALUE")` returning the quoted value.
fn parse_quoted_field(block: &str, prefix: &str) -> Option<String> {
    let pos = block.find(prefix)?;
    let after = &block[pos + prefix.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Parse `(layers "L1" "L2" ...)` returning all layer names.
fn parse_layers_field(block: &str) -> Vec<String> {
    let mut layers = Vec::new();
    if let Some(pos) = block.find("(layers ") {
        let after = &block[pos + 8..];
        if let Some(close) = after.find(')') {
            let layer_str = &after[..close];
            let mut s = layer_str;
            while let Some(q) = s.find('"') {
                let inner = &s[q + 1..];
                if let Some(end) = inner.find('"') {
                    layers.push(inner[..end].to_string());
                    s = &inner[end + 1..];
                } else {
                    break;
                }
            }
        }
    }
    layers
}

/// Build a connectivity graph and BFS from pad A to pad B.
/// Returns Ok(true) if connected, Ok(false) if not, Err if pad not found.
fn check_pad_connectivity(
    pads: &[PcbPad],
    segments: &[PcbSegment],
    vias: &[PcbVia],
    ref_a: &str, pad_a: &str,
    ref_b: &str, pad_b: &str,
) -> Result<bool, String> {
    use std::collections::{HashMap, HashSet, VecDeque};

    // Find absolute positions of both pads
    let find = |r: &str, p: &str| -> Option<(f64, f64)> {
        pads.iter().find(|pad| pad.reference == r && pad.pad_num == p)
            .map(|pad| (pad.x, pad.y))
    };
    let (ax, ay) = find(ref_a, pad_a).ok_or_else(|| format!("pad {ref_a}/{pad_a} not found"))?;
    let (bx, by) = find(ref_b, pad_b).ok_or_else(|| format!("pad {ref_b}/{pad_b} not found"))?;

    // Node: (x_um, y_um, layer_index) — use layer as string key
    type Node = (i64, i64, String);
    let mut adj: HashMap<Node, Vec<Node>> = HashMap::new();

    let mut add_edge = |a: Node, b: Node| {
        adj.entry(a.clone()).or_default().push(b.clone());
        adj.entry(b).or_default().push(a);
    };

    for seg in segments {
        let a: Node = (coord_key(seg.x1), coord_key(seg.y1), seg.layer.clone());
        let b: Node = (coord_key(seg.x2), coord_key(seg.y2), seg.layer.clone());
        add_edge(a, b);
    }

    for via in vias {
        let vx = coord_key(via.x);
        let vy = coord_key(via.y);
        // Vias connect all layer pairs they span
        for l1 in &via.layers {
            for l2 in &via.layers {
                if l1 < l2 {
                    let a: Node = (vx, vy, l1.clone());
                    let b: Node = (vx, vy, l2.clone());
                    add_edge(a, b);
                }
            }
        }
    }

    // BFS — try starting from pad A on each copper layer
    let target_x = coord_key(bx);
    let target_y = coord_key(by);

    for start_layer in &["F.Cu", "B.Cu"] {
        let start: Node = (coord_key(ax), coord_key(ay), start_layer.to_string());
        if !adj.contains_key(&start) {
            continue;
        }
        let mut visited: HashSet<Node> = HashSet::new();
        let mut queue: VecDeque<Node> = VecDeque::new();
        queue.push_back(start.clone());
        visited.insert(start);

        while let Some(node) = queue.pop_front() {
            if node.0 == target_x && node.1 == target_y {
                return Ok(true);
            }
            if let Some(neighbors) = adj.get(&node) {
                for next in neighbors {
                    if !visited.contains(next) {
                        visited.insert(next.clone());
                        queue.push_back(next.clone());
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Find KiCad power symbol library file, return path if found.
fn find_power_symbol_lib() -> Option<std::path::PathBuf> {
    let candidates = [
        "/usr/share/kicad/symbols/power.kicad_sym",
        "/usr/local/share/kicad/symbols/power.kicad_sym",
    ];
    // Also check user-local kicad versions
    let mut paths: Vec<std::path::PathBuf> = candidates.iter().map(Into::into).collect();
    if let Ok(home) = std::env::var("HOME") {
        for ver in &["9.0", "8.0", "7.0"] {
            paths.push(format!("{home}/.local/share/kicad/{ver}/symbols/power.kicad_sym").into());
        }
    }
    paths.into_iter().find(|p| p.exists())
}

/// Extract the `(symbol "power:NET" ...)` block from a .kicad_sym library file.
fn extract_lib_symbol(lib_content: &str, net_name: &str) -> Option<String> {
    use pcb_edit::block_end;
    let marker = format!("(symbol \"power:{net_name}\"");
    let pos = lib_content.find(&marker)?;
    // Walk back to find the opening paren at the start of this symbol block
    let start = lib_content[..pos].rfind('\n').map(|p| p + 1).unwrap_or(pos);
    let end = block_end(lib_content, pos);
    // Rewrite lib_id form to embedded form: strip leading whitespace, keep inner content
    let raw = &lib_content[start..end];
    // Indent one level for lib_symbols embedding
    let indented: String = raw.lines()
        .map(|l| if l.is_empty() { String::new() } else { format!("\t\t{l}") })
        .collect::<Vec<_>>()
        .join("\n");
    Some(indented)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run() -> anyhow::Result<()> {
    let server = KiCadServer::new();
    server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .context("MCP server error")?
        .waiting()
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Regression tests for the tab-indented, multi-line schematic symbol format
// (real modern KiCad output) that several schematic-editing helpers used to
// silently fail to parse — see P0-8 in the MCP tool improvement plan.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally real tab-indented, multi-line schematic,
    /// mirroring the exact shape of modern KiCad output (and the real file
    /// that exposed these bugs): `(symbol` and `(lib_id ...)` on separate
    /// lines, a `(wire ...)` block, and a `(lib_symbols ...)` section.
    fn sample_schematic() -> String {
        "\
(kicad_sch
	(version 20231120)
	(lib_symbols
		(symbol \"Connector:USB_B_Mini\"
			(pin_names
				(offset 1.016)
			)
			(symbol \"USB_B_Mini_1_1\"
				(pin power_out line
					(at 7.62 5.08 180)
					(length 2.54)
					(name \"VBUS\")
					(number \"1\")
				)
			)
		)
	)
	(wire
		(pts
			(xy 72.39 53.34) (xy 74.93 53.34)
		)
		(stroke
			(width 0)
			(type default)
		)
		(uuid \"200f4132-b397-4ea7-b4b3-866ee1e7c310\")
	)
	(symbol
		(lib_id \"Connector:USB_B_Mini\")
		(at 74.93 43.18 0)
		(unit 1)
		(property \"Reference\" \"PWR1\"
			(at 76.3778 31.3182 0)
		)
		(property \"Value\" \"USB_B_Mini\"
			(at 76.3778 33.6296 0)
		)
		(property \"Footprint\" \"\"
			(at 78.74 44.45 0)
		)
		(instances
			(project \"test\"
				(path \"/abc\"
					(reference \"PWR1\")
					(unit 1)
				)
			)
		)
	)
)
".to_string()
    }

    #[test]
    fn find_symbol_blocks_matches_multiline_tab_indented_form() {
        let content = sample_schematic();
        let blocks = find_symbol_blocks(&content);
        assert!(
            blocks.contains_key("PWR1"),
            "expected to find reference PWR1 in a tab-indented, multi-line schematic; got keys: {:?}",
            blocks.keys().collect::<Vec<_>>()
        );
        // Must not have picked up the nested lib_symbols library definition as a
        // placed instance (it has no top-level "Reference" property at all, so it
        // simply shouldn't appear in the map under any key).
        assert_eq!(blocks.len(), 1, "expected exactly one placed instance, got: {:?}", blocks.keys().collect::<Vec<_>>());
    }

    #[test]
    fn find_sch_symbol_by_ref_finds_multiline_tab_indented_instance() {
        let content = sample_schematic();
        let range = find_sch_symbol_by_ref(&content, "PWR1");
        assert!(range.is_some(), "find_sch_symbol_by_ref failed on tab-indented, multi-line schematic");
        let block = &content[range.unwrap()];
        assert!(block.contains("\"PWR1\""));
        assert!(block.starts_with("(symbol"));
    }

    #[test]
    fn sch_replace_at_locates_at_on_its_own_line() {
        let content = sample_schematic();
        let range = find_sch_symbol_by_ref(&content, "PWR1").expect("block not found");
        let block = &content[range];
        let new_block = sch_replace_at(block, 100.0, 50.0, Some(90.0));
        assert!(new_block.contains("(at 100 50 90)"), "new placement not applied: {new_block}");
        // The rest of the block (reference property etc.) must be preserved.
        assert!(new_block.contains("\"PWR1\""));
    }

    #[test]
    fn compute_pin_positions_works_on_multiline_tab_indented_instance() {
        let content = sample_schematic();
        let pins = compute_pin_positions(&content, "PWR1");
        assert!(
            !pins.is_empty(),
            "expected at least one pin position for PWR1 in a tab-indented, multi-line schematic"
        );
    }

    /// Regression test for P0-10: the "context after edit" preview used to
    /// re-search the whole file for new_string's first line, which could latch
    /// onto an earlier, unrelated occurrence of generic/repeated boilerplate
    /// rather than the actual edit location. This builds a file with two
    /// structurally-similar blocks sharing a generic first line, edits the
    /// SECOND one via a globally-unique old_string, and confirms the preview
    /// shows the second block's location (with its updated content), not the
    /// untouched first block.
    #[tokio::test]
    async fn patch_kicad_file_context_preview_shows_real_edit_location() {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("p0_10_test.kicad_pcb");
        // Two pads share the generic first line "(pad \"1\" thru_hole circle" —
        // exactly the kind of repeated boilerplate that broke the old preview.
        let content = "\
(kicad_pcb
\t(footprint \"A\"
\t\t(pad \"1\" thru_hole circle
\t\t\t(at 1 1)
\t\t\t(uuid \"aaaa\")
\t\t)
\t)
\t(footprint \"B\"
\t\t(pad \"1\" thru_hole circle
\t\t\t(at 99 99)
\t\t\t(uuid \"bbbb-unique\")
\t\t)
\t)
)
";
        std::fs::write(&file_path, content).unwrap();

        let server = KiCadServer::new();
        let params = Parameters(PatchKicadParams {
            path: file_path.to_str().unwrap().to_string(),
            old_string: "\t\t(pad \"1\" thru_hole circle\n\t\t\t(at 99 99)\n\t\t\t(uuid \"bbbb-unique\")".to_string(),
            new_string: "\t\t(pad \"1\" thru_hole circle\n\t\t\t(at 42 42)\n\t\t\t(uuid \"bbbb-unique\")".to_string(),
            replace_all: None,
            render_preview: Some(false),
        });
        let result = server.patch_kicad_file(params).await.expect("call failed");
        let text = result.content.iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!result.is_error.unwrap_or(false), "patch_kicad_file returned an error: {text}");
        assert!(text.contains("(at 42 42)"), "preview should show the actual edited block (footprint B, now at 42 42): {text}");
        assert!(!text.contains("(at 1 1)"), "preview should NOT show footprint A's untouched block: {text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_dangling_wires_finds_tab_indented_multiline_wire_blocks() {
        let content = sample_schematic();
        // This wire's far endpoint (72.39, 53.34) is genuinely dangling (nothing
        // else references that point) and its near endpoint (74.93, 53.34) is not
        // anchored to anything in this minimal fixture either, so the whole wire
        // should be detected and removed — the key regression check is that the
        // wire is found at all (count != 0), not the specific anchor logic.
        let (_new_content, count) = remove_dangling_wires(&content);
        assert_eq!(count, 1, "expected to find and remove exactly 1 dangling wire in the tab-indented fixture");
    }

    /// Minimal PCB fixture with a single existing F.Cu trace on net "TX",
    /// mirroring the exact TX/DATA/CLK crossing scenario from the real session
    /// that motivated P0-1.
    fn sample_pcb_with_trace() -> String {
        "\
(kicad_pcb
\t(segment
\t\t(start 10 10)
\t\t(end 20 10)
\t\t(width 0.4)
\t\t(layer \"F.Cu\")
\t\t(net \"TX\")
\t)
)
".to_string()
    }

    #[test]
    fn parse_pcb_segments_finds_tab_indented_segment_with_net_and_width() {
        let content = sample_pcb_with_trace();
        let segs = parse_pcb_segments(&content);
        assert_eq!(segs.len(), 1, "expected exactly one parsed segment");
        assert_eq!(segs[0].net, "TX");
        assert!((segs[0].width - 0.4).abs() < 1e-9);
        assert_eq!(segs[0].layer, "F.Cu");
    }

    #[test]
    fn compute_clearance_flags_different_net_same_layer_crossing() {
        let content = sample_pcb_with_trace();
        // Proposed segment crosses the existing TX trace at (15, 10), different net.
        let (collisions, _warnings) = compute_clearance(
            &content, 15.0, 5.0, 15.0, 15.0, "F.Cu", 0.4, 0.1, "DATA",
        );
        assert!(!collisions.is_empty(), "expected a collision for a different-net trace crossing on the same layer");
    }

    #[test]
    fn compute_clearance_allows_same_net_t_junction() {
        let content = sample_pcb_with_trace();
        // Same crossing geometry, but proposed net matches the existing trace's net.
        let (collisions, _warnings) = compute_clearance(
            &content, 15.0, 5.0, 15.0, 15.0, "F.Cu", 0.4, 0.1, "TX",
        );
        assert!(collisions.is_empty(), "same-net T-junction must not be flagged as a collision");
    }

    #[test]
    fn compute_clearance_ignores_different_layer_traces() {
        let content = sample_pcb_with_trace();
        // Identical crossing geometry, but on B.Cu — must not collide with the F.Cu trace.
        let (collisions, warnings) = compute_clearance(
            &content, 15.0, 5.0, 15.0, 15.0, "B.Cu", 0.4, 0.1, "DATA",
        );
        assert!(collisions.is_empty() && warnings.is_empty(), "different-layer traces must never collide");
    }

    #[test]
    fn compute_clearance_checks_two_unrouted_segments_against_each_other() {
        let content = sample_pcb_with_trace();
        // An unrouted (no-net) proposed segment crossing the existing trace's net
        // is a real collision, but let's specifically verify the empty-net case
        // isn't treated as a same-net wildcard against an unrouted EXISTING
        // segment either — the exact failure mode this check exists to catch.
        let content_two_unrouted = "\
(kicad_pcb
\t(segment
\t\t(start 10 10)
\t\t(end 20 10)
\t\t(width 0.4)
\t\t(layer \"F.Cu\")
\t\t(net \"\")
\t)
)
".to_string();
        let _ = content; // unused in this variant
        let (collisions, _warnings) = compute_clearance(
            &content_two_unrouted, 15.0, 5.0, 15.0, 15.0, "F.Cu", 0.4, 0.1, "",
        );
        assert!(!collisions.is_empty(), "two unrouted segments touching must still be flagged as a collision");
    }

    #[tokio::test]
    async fn add_trace_check_true_refuses_on_collision_and_force_overrides() {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_at_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("p1_6_test.kicad_pcb");
        std::fs::write(&file_path, sample_pcb_with_trace()).unwrap();

        let server = KiCadServer::new();

        // check=true, colliding path (crosses the existing TX trace on a different net) → refused, no write.
        let before = std::fs::read_to_string(&file_path).unwrap();
        let params = Parameters(AddTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 15.0, y1: 5.0, x2: 15.0, y2: 15.0,
            layer: "F.Cu".to_string(), width: Some(0.4), net: Some("DATA".to_string()),
            check: Some(true), force: None,
        });
        let result = server.add_trace(params).await.expect("call failed");
        assert!(result.is_error.unwrap_or(false), "expected refusal on collision");
        let after = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(before, after, "file must be unchanged when the write is refused");

        // Same colliding path with force=true → writes anyway.
        let params = Parameters(AddTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 15.0, y1: 5.0, x2: 15.0, y2: 15.0,
            layer: "F.Cu".to_string(), width: Some(0.4), net: Some("DATA".to_string()),
            check: Some(true), force: Some(true),
        });
        let result = server.add_trace(params).await.expect("call failed");
        assert!(!result.is_error.unwrap_or(false), "force=true must write despite the collision");
        let after = std::fs::read_to_string(&file_path).unwrap();
        assert_ne!(before, after, "file must change once force=true is set");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn add_trace_check_omitted_preserves_always_write_behavior() {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_at2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("p1_6_test2.kicad_pcb");
        std::fs::write(&file_path, sample_pcb_with_trace()).unwrap();

        let server = KiCadServer::new();
        let before = std::fs::read_to_string(&file_path).unwrap();

        // Colliding path, but check is omitted entirely — must write unconditionally (backward compat).
        let params = Parameters(AddTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 15.0, y1: 5.0, x2: 15.0, y2: 15.0,
            layer: "F.Cu".to_string(), width: Some(0.4), net: Some("DATA".to_string()),
            check: None, force: None,
        });
        let result = server.add_trace(params).await.expect("call failed");
        assert!(!result.is_error.unwrap_or(false), "omitted check must never block a write");
        let after = std::fs::read_to_string(&file_path).unwrap();
        assert_ne!(before, after, "trace must be written when check is omitted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn segment_bbox_overlaps_region_basic_cases() {
        let seg = PcbSegment { x1: 10.0, y1: 10.0, x2: 20.0, y2: 10.0, layer: "F.Cu".into(), net: "TX".into(), width: 0.4 };
        // Region fully containing the segment
        assert!(segment_bbox_overlaps_region(&seg, 0.0, 0.0, 30.0, 30.0));
        // Region only clipping the segment's bbox corner
        assert!(segment_bbox_overlaps_region(&seg, 15.0, 5.0, 25.0, 15.0));
        // Region entirely to the side, no overlap
        assert!(!segment_bbox_overlaps_region(&seg, 50.0, 50.0, 60.0, 60.0));
    }

    #[tokio::test]
    async fn query_traces_in_region_finds_and_filters_by_net_and_layer() {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_qtir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("p0_2_test.kicad_pcb");
        std::fs::write(&file_path, sample_pcb_with_trace()).unwrap();

        let server = KiCadServer::new();

        // Region containing the trace, no filters — should find it.
        let params = Parameters(QueryTracesInRegionParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 0.0, y1: 0.0, x2: 30.0, y2: 30.0,
            layer: None, net: None,
        });
        let result = server.query_traces_in_region(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("TX"), "expected the TX trace in the region: {text}");
        assert!(text.contains("Total: 1 traces"), "expected exactly one match: {text}");

        // Wrong net filter — should find nothing.
        let params = Parameters(QueryTracesInRegionParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 0.0, y1: 0.0, x2: 30.0, y2: 30.0,
            layer: None, net: Some("GND".to_string()),
        });
        let result = server.query_traces_in_region(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("No traces in region"), "expected no matches for a non-matching net filter: {text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// PCB fixture with an F.Cu-only SMD pad, a B.Cu-only SMD pad, and a THT pad, for
    /// query_pads_in_region layer-filter tests (P1-5).
    fn sample_pcb_with_layered_pads() -> String {
        "\
(kicad_pcb
\t(footprint \"Fp\"
\t\t(property \"Reference\" \"U1\" (at 0 0 0))
\t\t(at 0 0)
\t\t(pad \"1\" smd rect (at 5 5) (size 1 1) (layers \"F.Cu\" \"F.Paste\" \"F.Mask\") (net \"A\"))
\t\t(pad \"2\" smd rect (at 6 5) (size 1 1) (layers \"B.Cu\" \"B.Paste\" \"B.Mask\") (net \"B\"))
\t\t(pad \"3\" thru_hole circle (at 7 5) (size 1 1) (drill 0.5) (layers \"*.Cu\" \"*.Mask\") (net \"C\"))
\t)
)
".to_string()
    }

    #[tokio::test]
    async fn query_pads_in_region_layer_filter_excludes_non_matching_smd_pads() {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_qpir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("p1_5_test.kicad_pcb");
        std::fs::write(&file_path, sample_pcb_with_layered_pads()).unwrap();

        let server = KiCadServer::new();

        // Filter by F.Cu: should return the F.Cu SMD pad and the THT pad, but not the B.Cu SMD pad.
        let params = Parameters(QueryPadsInRegionParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0,
            layer: Some("F.Cu".to_string()),
        });
        let result = server.query_pads_in_region(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("U1         1"), "F.Cu pad should match: {text}");
        assert!(text.contains("U1         3"), "THT pad should match any layer filter: {text}");
        assert!(!text.contains("U1         2"), "B.Cu-only pad must not match an F.Cu filter: {text}");

        // No filter: all three pads returned.
        let params = Parameters(QueryPadsInRegionParams {
            path: file_path.to_str().unwrap().to_string(),
            x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0,
            layer: None,
        });
        let result = server.query_pads_in_region(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Total: 3 pads"), "expected all 3 pads with no filter: {text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// PCB fixture with two segments on different nets, for delete_trace tests.
    fn sample_pcb_two_traces() -> String {
        "\
(kicad_pcb
\t(segment
\t\t(start 10 10)
\t\t(end 20 10)
\t\t(width 0.4)
\t\t(layer \"F.Cu\")
\t\t(net \"TX\")
\t\t(uuid \"tx-uuid-1234\")
\t)
\t(segment
\t\t(start 50 50)
\t\t(end 60 50)
\t\t(width 0.4)
\t\t(layer \"B.Cu\")
\t\t(net \"GND\")
\t\t(uuid \"gnd-uuid-5678\")
\t)
)
".to_string()
    }

    async fn write_test_pcb(name: &str, content: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kicad2print_test_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test.kicad_pcb");
        std::fs::write(&file_path, content).unwrap();
        (dir, file_path)
    }

    #[tokio::test]
    async fn delete_trace_by_net_removes_only_matching_net() {
        let (dir, file_path) = write_test_pcb("dt_net", &sample_pcb_two_traces()).await;
        let server = KiCadServer::new();
        let params = Parameters(DeleteTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            net: Some("TX".to_string()), layer: None, uuid: None,
            x1: None, y1: None, x2: None, y2: None, dry_run: None,
        });
        let result = server.delete_trace(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Removed 1 trace segment"), "expected exactly 1 removed: {text}");

        let remaining = std::fs::read_to_string(&file_path).unwrap();
        assert!(!remaining.contains("\"TX\""), "TX segment should be gone");
        assert!(remaining.contains("\"GND\""), "GND segment should remain untouched");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_trace_dry_run_reports_without_writing() {
        let (dir, file_path) = write_test_pcb("dt_dryrun", &sample_pcb_two_traces()).await;
        let original = std::fs::read_to_string(&file_path).unwrap();
        let server = KiCadServer::new();
        let params = Parameters(DeleteTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            net: Some("TX".to_string()), layer: None, uuid: None,
            x1: None, y1: None, x2: None, y2: None, dry_run: Some(true),
        });
        let result = server.delete_trace(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("DRY RUN"), "expected a dry-run report: {text}");
        assert!(text.contains("net=TX"), "expected the match description to mention the net: {text}");

        let after = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(original, after, "dry_run must not modify the file");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_trace_by_uuid_removes_exactly_one() {
        let (dir, file_path) = write_test_pcb("dt_uuid", &sample_pcb_two_traces()).await;
        let server = KiCadServer::new();
        let params = Parameters(DeleteTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            net: None, layer: None, uuid: Some("gnd-uuid-5678".to_string()),
            x1: None, y1: None, x2: None, y2: None, dry_run: None,
        });
        let result = server.delete_trace(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Removed 1 trace segment"), "expected exactly 1 removed by uuid: {text}");

        let remaining = std::fs::read_to_string(&file_path).unwrap();
        assert!(!remaining.contains("gnd-uuid-5678"));
        assert!(remaining.contains("tx-uuid-1234"), "TX segment should remain untouched");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_trace_rejects_call_with_no_filters() {
        let (dir, file_path) = write_test_pcb("dt_nofilter", &sample_pcb_two_traces()).await;
        let server = KiCadServer::new();
        let params = Parameters(DeleteTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            net: None, layer: None, uuid: None,
            x1: None, y1: None, x2: None, y2: None, dry_run: None,
        });
        let result = server.delete_trace(params).await.expect("call failed");
        assert!(result.is_error.unwrap_or(false), "expected an error when no filter is provided");

        let unchanged = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(unchanged, sample_pcb_two_traces(), "file must be untouched when the call is rejected");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_trace_by_region_only_removes_overlapping_segment() {
        let (dir, file_path) = write_test_pcb("dt_region", &sample_pcb_two_traces()).await;
        let server = KiCadServer::new();
        // Region around the TX segment (10,10)-(20,10) only.
        let params = Parameters(DeleteTraceParams {
            path: file_path.to_str().unwrap().to_string(),
            net: None, layer: None, uuid: None,
            x1: Some(0.0), y1: Some(0.0), x2: Some(30.0), y2: Some(30.0),
            dry_run: None,
        });
        let result = server.delete_trace(params).await.expect("call failed");
        let text = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Removed 1 trace segment"), "expected exactly 1 removed by region: {text}");

        let remaining = std::fs::read_to_string(&file_path).unwrap();
        assert!(!remaining.contains("\"TX\""), "TX segment inside the region should be gone");
        assert!(remaining.contains("\"GND\""), "GND segment outside the region should remain");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coord_key_absorbs_submicron_drift_but_not_real_separation() {
        // Sub-micron float drift (well under 5 microns) must bucket identically —
        // this is the exact false-negative this fix addresses.
        assert_eq!(coord_key(10.0), coord_key(10.0 + 0.0000003));
        // Two genuinely distinct points 0.05mm apart (well below any realistic
        // KiCad clearance, but clearly not the same point) must NOT bucket
        // identically — confirms the fix doesn't introduce false positives.
        assert_ne!(coord_key(10.0), coord_key(10.05));
    }

    #[test]
    fn check_pad_connectivity_no_longer_false_negatives_on_submicron_drift() {
        let pads = vec![
            PcbPad { reference: "U1".into(), pad_num: "1".into(), net: "TX".into(), x: 10.0, y: 10.0, width: 1.7, height: 1.7, is_thru_hole: true, layers: vec!["F.Cu".to_string()] },
            PcbPad { reference: "U2".into(), pad_num: "1".into(), net: "TX".into(), x: 20.0, y: 10.0, width: 1.7, height: 1.7, is_thru_hole: true, layers: vec!["F.Cu".to_string()] },
        ];
        // Trace endpoint independently computed via rotation trig differs from
        // the pad center by a fraction of a micron — exactly the real-world
        // scenario that caused a false DISCONNECTED report before this fix.
        let segments = vec![
            PcbSegment { x1: 10.0 + 0.0000004, y1: 10.0, x2: 20.0 - 0.0000004, y2: 10.0, layer: "F.Cu".into(), net: "TX".into(), width: 0.4 },
        ];
        let result = check_pad_connectivity(&pads, &segments, &[], "U1", "1", "U2", "1");
        assert_eq!(result, Ok(true), "sub-micron drift must not cause a false DISCONNECTED result");
    }

    #[test]
    fn check_pad_connectivity_still_reports_disconnected_when_truly_unrouted() {
        let pads = vec![
            PcbPad { reference: "U1".into(), pad_num: "1".into(), net: "TX".into(), x: 10.0, y: 10.0, width: 1.7, height: 1.7, is_thru_hole: true, layers: vec!["F.Cu".to_string()] },
            PcbPad { reference: "U2".into(), pad_num: "1".into(), net: "TX".into(), x: 20.0, y: 10.0, width: 1.7, height: 1.7, is_thru_hole: true, layers: vec!["F.Cu".to_string()] },
        ];
        let result = check_pad_connectivity(&pads, &[], &[], "U1", "1", "U2", "1");
        assert_eq!(result, Ok(false), "genuinely unrouted pads must still report DISCONNECTED");
    }
}
