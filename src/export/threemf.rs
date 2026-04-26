//! 3MF writer.
//!
//! 3MF is a ZIP archive containing:
//!   [Content_Types].xml
//!   _rels/.rels
//!   3D/3dmodel.model  ← the mesh as an XML vertex/triangle list

use crate::geometry::Mesh3D;
use anyhow::Result;
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

pub fn write(mesh: &Mesh3D, path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(CONTENT_TYPES.as_bytes())?;

    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(RELS.as_bytes())?;

    zip.start_file("3D/3dmodel.model", opts)?;
    write_model(&mut zip, mesh)?;

    zip.finish()?;
    Ok(())
}

fn write_model<W: Write>(w: &mut W, mesh: &Mesh3D) -> Result<()> {
    write!(
        w,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
<resources><object id="1" type="model"><mesh>
<vertices>
"#
    )?;

    for tri in &mesh.triangles {
        for v in &tri.vertices {
            writeln!(w, r#"<vertex x="{:.4}" y="{:.4}" z="{:.4}"/>"#, v[0], v[1], v[2])?;
        }
    }

    writeln!(w, "</vertices>")?;
    writeln!(w, "<triangles>")?;

    for i in 0..mesh.triangles.len() {
        let b = i * 3;
        writeln!(w, r#"<triangle v1="{}" v2="{}" v3="{}"/>"#, b, b + 1, b + 2)?;
    }

    write!(
        w,
        "</triangles>\n</mesh></object></resources>\n<build><item objectid=\"1\"/></build>\n</model>"
    )?;
    Ok(())
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>"#;
