// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0

//! MCP server — exposes kicad2print as a Model Context Protocol tool.

use anyhow::Context as _;
use base64::{Engine, prelude::BASE64_STANDARD};
use rmcp::{
    ErrorData as McpError,
    ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerInfo},
    tool, tool_handler, tool_router,
    schemars,
    ServerHandler,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::process::Command;

use crate::{autoscale, config::Config, export, geometry, parser, render};

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadPcbParams {
    /// Absolute or relative path to the .kicad_pcb (or .kicad_sch) file to read
    pub path: String,
}

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
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct KiCadServer {
    tool_router: ToolRouter<Self>,
    /// Candidate directories to search for footprint .pretty libraries
    fp_lib_dirs: Vec<PathBuf>,
}

impl KiCadServer {
    pub fn new() -> Self {
        let fp_lib_dirs = footprint_library_search_dirs();
        Self {
            tool_router: Self::tool_router(),
            fp_lib_dirs,
        }
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

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl KiCadServer {
    // ---- File I/O ----------------------------------------------------------

    /// Read the raw S-expression content of a KiCad file (.kicad_pcb or .kicad_sch).
    /// Returns the full text so it can be inspected or used as a base for edits.
    #[tool(description = "Read a KiCad file (.kicad_pcb or .kicad_sch) and return its raw S-expression content")]
    async fn read_kicad_file(
        &self,
        params: Parameters<ReadPcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&params.0.path);
        match fs::read_to_string(&path).await {
            Ok(content) => Ok(CallToolResult::success(vec![Content::text(content)])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read {}: {e}", path.display()
            ))])),
        }
    }

    /// Write S-expression content to a KiCad file (create or overwrite).
    /// After writing a .kicad_pcb, call render_pcb or run_drc to check the result.
    #[tool(description = "Write KiCad S-expression content to a file (.kicad_pcb or .kicad_sch) — create or update a design")]
    async fn write_kicad_file(
        &self,
        params: Parameters<WritePcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&params.0.path);
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
    /// Returns the PNG image directly so the board can be inspected visually.
    #[tool(description = "Render a .kicad_pcb file to a 3D preview PNG using KiCad's raytracer (top/bottom/side views available)")]
    async fn render_pcb(
        &self,
        params: Parameters<RenderPcbParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let out_path = std::env::temp_dir().join(format!(
            "kicad_render_{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let side = p.side.as_deref().unwrap_or("top");
        let width = p.width.unwrap_or(1200).to_string();
        let height = p.height.unwrap_or(800).to_string();
        let quality = p.quality.as_deref().unwrap_or("high");

        let (_, stderr, code) = run_kicad_cli(&[
            "pcb", "render",
            "--output", out_path.to_str().unwrap_or("/tmp/render.png"),
            "--width", &width,
            "--height", &height,
            "--side", side,
            "--quality", quality,
            "--background", "opaque",
            &p.path,
        ]).await?;

        if code != 0 {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "kicad-cli render failed (exit {code}):\n{stderr}"
            ))]));
        }

        match fs::read(&out_path).await {
            Ok(bytes) => {
                let _ = fs::remove_file(&out_path).await;
                Ok(CallToolResult::success(vec![
                    Content::text(format!("Rendered {} view of {}", side, p.path)),
                    Content::image(BASE64_STANDARD.encode(&bytes), "image/png"),
                ]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Render succeeded but could not read output file: {e}\nstderr: {stderr}"
            ))])),
        }
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
                Ok(CallToolResult::success(vec![Content::text(report)]))
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

        let all_libs = collect_all_pretty_dirs(&self.fp_lib_dirs, None).await;

        if all_libs.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No footprint libraries found on this system.\n\
                 Install with: sudo pacman -S kicad-library"
                    .to_string(),
            )]));
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
    #[tool(description = "Scan a folder for a KiCad project — lists all PCB/schematic files, exports BOM, and renders the board(s) for immediate visual context")]
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
            let out_path = std::env::temp_dir().join(format!(
                "kicad_scan_render_{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ));
            let (_, _, code) = run_kicad_cli(&[
                "pcb", "render",
                "--output", out_path.to_str().unwrap_or("/tmp/render.png"),
                "--width", "1200", "--height", "800",
                "--side", "top", "--quality", "basic",
                "--background", "opaque",
                pcb.to_str().unwrap_or(""),
            ]).await?;
            if code == 0 {
                if let Ok(bytes) = fs::read(&out_path).await {
                    let _ = fs::remove_file(&out_path).await;
                    contents.push(Content::text(format!(
                        "PCB render — {}:", pcb.display()
                    )));
                    contents.push(Content::image(BASE64_STANDARD.encode(&bytes), "image/png"));
                }
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

        match fs::read_to_string(&out_path).await {
            Ok(svg) => {
                let _ = fs::remove_file(&out_path).await;
                Ok(CallToolResult::success(vec![Content::text(svg)]))
            }
            Err(_) => Ok(CallToolResult::error(vec![Content::text(format!(
                "SVG export failed (exit {code}):\n{stderr}"
            ))])),
        }
    }

    // ---- kicad2print 3D substrate conversion -------------------------------

    /// Convert a KiCad PCB to a 3D-printable substrate (STL/3MF) using kicad2print.
    /// Returns a software-rendered preview of the substrate geometry.
    #[tool(description = "Convert a .kicad_pcb file to a 3D-printable substrate model (STL/3MF) and return a preview image")]
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

        let pcb = parser::parse_pcb(&input)
            .context("Failed to parse KiCad PCB file")
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let scale = autoscale::compute_scale_factor(&pcb, &config);
        let pcb = if (scale - 1.0).abs() > 0.001 { pcb.scale(scale) } else { pcb };

        let mesh = geometry::generate_model(&pcb, &config)
            .context("Failed to generate 3D geometry")
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let tri_count = mesh.triangle_count();

        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("board");
        let written = export::export(&mesh, &pcb, stem, &config)
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
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for KiCadServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new("kicad2print", "0.1.0");
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
