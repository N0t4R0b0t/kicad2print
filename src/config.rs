//! Configuration management for `kicad2print`.
//!
//! This module handles loading settings from TOML config files and merging them with
//! command-line argument overrides. The configuration drives the geometry generation
//! and output options.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

    /// How vias are represented in the 3D model.
    ///
    /// - "hole": full through-holes that you can drill with a drill press
    /// - "indent": shallow dimples on top and bottom (no drilling required)
    /// Default: "indent" (faster printing and assembly)
    #[serde(default = "default_eyelet_style")]
    pub eyelet_style: EyeletStyle,

    /// Diameter of the via holes or indent dimples.
    ///
    /// Should match the size of your copper eyelets (typically M0.9 or M1.3).
    /// Default: 1.5 mm (good for standard eyelets)
    #[serde(default = "default_eyelet_diameter")]
    pub eyelet_diameter_mm: f64,

    /// Depth of shallow indent dimples (only used when eyelet_style = "indent").
    ///
    /// A shallower indent is easier to print but provides less guidance for the eyelet.
    /// Default: 0.3 mm (visible indentation but doesn't weaken substrate)
    #[serde(default = "default_indent_depth")]
    pub indent_depth_mm: f64,

    /// Diameter of component pad through-holes.
    ///
    /// Should be slightly larger than the component lead diameter.
    /// Default: 0.8 mm (good for standard component leads)
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
}

// Default value functions for serde
fn default_channel_width() -> f64 { 1.2 }
fn default_channel_depth() -> f64 { 0.5 }
fn default_eyelet_style() -> EyeletStyle { EyeletStyle::Indent }
fn default_eyelet_diameter() -> f64 { 1.5 }
fn default_indent_depth() -> f64 { 0.3 }
fn default_pad_hole_diameter() -> f64 { 0.8 }
fn default_substrate_thickness() -> f64 { 3.0 }
fn default_scale_factor() -> f64 { 0.0 }
fn default_output_format() -> OutputFormat { OutputFormat::Stl }
fn default_output_dir() -> String { "./output".to_string() }

impl Default for Config {
    /// Creates a config with all default values.
    fn default() -> Self {
        Config {
            channel_width_mm: default_channel_width(),
            channel_depth_mm: default_channel_depth(),
            eyelet_style: default_eyelet_style(),
            eyelet_diameter_mm: default_eyelet_diameter(),
            indent_depth_mm: default_indent_depth(),
            pad_hole_diameter_mm: default_pad_hole_diameter(),
            substrate_thickness_mm: default_substrate_thickness(),
            scale_factor: default_scale_factor(),
            output_format: default_output_format(),
            output_dir: default_output_dir(),
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
    }

    /// Prints the current configuration to stdout.
    ///
    /// Useful for debugging to confirm which settings are being used.
    pub fn print_summary(&self) {
        println!("=== Configuration ===");
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
    }
}

/// Command-line argument overrides.
///
/// This struct mirrors Config but with `Option` fields so that unspecified
/// arguments don't override config file values.
#[derive(Debug, Default)]
pub struct CliOverrides {
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
}
