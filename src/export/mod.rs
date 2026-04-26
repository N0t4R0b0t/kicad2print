//! Export orchestration — writes the mesh to STL and/or 3MF files.

pub mod stl;
pub mod threemf;

use crate::config::{Config, OutputFormat};
use crate::geometry::Mesh3D;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Write `mesh` to one or both output formats as configured.
/// Returns the list of files that were created.
pub fn export(mesh: &Mesh3D, stem: &str, config: &Config) -> Result<Vec<PathBuf>> {
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

    Ok(written)
}
