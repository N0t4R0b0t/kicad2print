// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Binary STL writer.
//!
//! Format: 80-byte header, uint32 triangle count, then for each triangle:
//!   3×float32 normal + 3×(3×float32) vertices + uint16 attribute (0).
//! Total: 50 bytes per triangle.

use crate::geometry::Mesh3D;
use anyhow::Result;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn write(mesh: &Mesh3D, path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut w = BufWriter::new(file);

    // 80-byte header
    let mut header = [0u8; 80];
    let tag = b"kicad2print";
    header[..tag.len()].copy_from_slice(tag);
    w.write_all(&header)?;

    // Triangle count
    w.write_all(&(mesh.triangles.len() as u32).to_le_bytes())?;

    for tri in &mesh.triangles {
        for &n in &tri.normal {
            w.write_all(&n.to_le_bytes())?;
        }
        for vertex in &tri.vertices {
            for &coord in vertex {
                w.write_all(&coord.to_le_bytes())?;
            }
        }
        w.write_all(&0u16.to_le_bytes())?;
    }

    Ok(())
}
