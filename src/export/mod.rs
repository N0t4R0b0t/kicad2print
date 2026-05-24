// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Export orchestration — writes the mesh to STL, 3MF, and HTML preview files.

pub mod assembly;
pub mod html;
pub mod stl;
pub mod threemf;

use crate::config::{Config, OutputFormat};
use crate::geometry::Mesh3D;
use crate::pcb::PcbData;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Write `mesh` to one or both output formats as configured, plus an HTML preview.
/// Returns the list of files that were created.
pub fn export(mesh: &Mesh3D, pcb: &PcbData, pcb_input: &Path, stem: &str, config: &Config) -> Result<Vec<PathBuf>> {
    let out_dir = Path::new(&config.output_dir);
    fs::create_dir_all(out_dir)?;

    let mut written = Vec::new();

    let write_stl = matches!(config.output_format, OutputFormat::Stl | OutputFormat::Both);
    let write_3mf = matches!(config.output_format, OutputFormat::ThreeM | OutputFormat::Both);

    if write_stl {
        let path = out_dir.join(format!("{}.stl", stem));
        stl::write(mesh, &path)?;
        written.push(path);
    }

    if write_3mf {
        let path = out_dir.join(format!("{}.3mf", stem));
        threemf::write(mesh, &path)?;
        written.push(path);
    }

    // Always generate HTML preview
    let html_path = out_dir.join(format!("{}_preview.html", stem));
    html::write(mesh, stem, &html_path)?;
    written.push(html_path);

    // Generate unified build guide (assembly + continuity + 3D tabs)
    let guide_path = out_dir.join(format!("{}_guide.html", stem));
    assembly::write(pcb, pcb_input, &config.assembly_steps, config.mode, stem, &guide_path)?;
    written.push(guide_path);

    Ok(written)
}
