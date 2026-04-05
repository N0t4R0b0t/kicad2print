//! PCB design parsing module.
//!
//! This module provides the complete parsing pipeline: from reading a KiCad file,
//! through S-expression parsing, to extracting meaningful PCB design elements.
//!
//! The main entry point is `parse_pcb()`.

pub mod kicad;
pub mod sexp;

use crate::pcb::PcbData;
use anyhow::{Context, Result};
use std::path::Path;

/// Parses a KiCad `.kicad_pcb` file and returns the extracted PCB design data.
///
/// This function orchestrates the complete parsing pipeline:
/// 1. Reads the file from disk
/// 2. Parses the S-expression format
/// 3. Walks the expression tree extracting design elements
/// 4. Returns a structured `PcbData` object
///
/// # Arguments
/// * `path` - Path to the `.kicad_pcb` file
///
/// # Example
/// ```no_run
/// let pcb_data = parse_pcb("my_board.kicad_pcb")?;
/// pcb_data.print_summary();
/// ```
///
/// # Errors
/// Returns an error if:
/// - The file cannot be read
/// - The file content is not valid S-expressions
/// - Required design elements are missing or malformed
pub fn parse_pcb<P: AsRef<Path>>(path: P) -> Result<PcbData> {
    let path = path.as_ref();

    // Step 1: Read the file
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read KiCad file: {}", path.display()))?;

    // Step 2: Parse S-expressions
    let sexp_nodes = sexp::parse_sexp(&content)
        .context("Failed to parse S-expressions from KiCad file")?;

    // Step 3: Extract PCB data from the expression tree
    let pcb_data = kicad::walk_kicad_tree(&sexp_nodes)
        .context("Failed to extract PCB design elements")?;

    Ok(pcb_data)
}
