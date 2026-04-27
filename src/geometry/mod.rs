// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! 3D geometry generation from parsed PCB data.
//!
//! Converts PCB traces, vias, and pads into a triangle mesh suitable for
//! 3D printing. The substrate is a solid slab with:
//!   - Grooved channels on the top face for F.Cu traces
//!   - Grooved channels on the bottom face for B.Cu traces
//!   - Through-holes for component pads
//!
//! All geometry is output in millimeters, with the board's minimum-corner
//! translated to the XY origin so the model starts at (0, 0, 0).
//!
//! Winding convention (right-hand rule):
//!   - Faces with upward (+Z) normals use CCW vertex order.
//!   - Faces with downward (−Z) normals use CW vertex order.
//!   - Side walls facing outward follow the right-of-travel rule for the
//!     CCW exterior ring.

use anyhow::{anyhow, Result};
use geo::{BooleanOps, Coord, LineString, MultiPolygon, Polygon};

use crate::config::Config;
use crate::pcb::{BoardOutline, Pad, PcbData, Point2, Trace};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Triangle3D {
    pub normal: [f32; 3],
    pub vertices: [[f32; 3]; 3],
}

#[derive(Debug, Clone, Default)]
pub struct Mesh3D {
    pub triangles: Vec<Triangle3D>,
}

impl Mesh3D {
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    fn tri(&mut self, v0: [f32; 3], v1: [f32; 3], v2: [f32; 3]) {
        let e1 = sub(v1, v0);
        let e2 = sub(v2, v0);
        let n = cross(e1, e2);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let normal = if len < 1e-10 {
            [0.0f32, 0.0, 1.0]
        } else {
            [n[0] / len, n[1] / len, n[2] / len]
        };
        self.triangles.push(Triangle3D {
            normal,
            vertices: [v0, v1, v2],
        });
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

// ---------------------------------------------------------------------------
// Coordinate helper (applies board-to-origin offset)
// ---------------------------------------------------------------------------

struct Ctx {
    ox: f64,
    oy: f64,
}

impl Ctx {
    fn v(&self, x: f64, y: f64, z: f32) -> [f32; 3] {
        [(x - self.ox) as f32, (y - self.oy) as f32, z]
    }

    fn coord(&self, c: &Coord, z: f32) -> [f32; 3] {
        self.v(c.x, c.y, z)
    }

    fn point(&self, p: &Point2, z: f32) -> [f32; 3] {
        self.v(p.x, p.y, z)
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub fn generate_model(pcb: &PcbData, config: &Config) -> Result<Mesh3D> {
    let mut mesh = Mesh3D::default();

    let outline = pcb
        .outline
        .as_ref()
        .ok_or_else(|| anyhow!("No board outline found — cannot generate 3D model"))?;

    // Sanity check: warn if channel depth leaves insufficient material
    if config.channel_depth_mm > config.substrate_thickness_mm / 2.0 {
        eprintln!(
            "⚠️  Warning: channel depth ({:.2}mm) exceeds half substrate thickness ({:.2}mm)",
            config.channel_depth_mm, config.substrate_thickness_mm
        );
        eprintln!(
            "   The substrate core may be too thin to hold wires securely or support eyelets."
        );
    }

    // Sanity check: warn if eyelet indent depth is very shallow
    if config.eyelet_style == crate::config::EyeletStyle::Indent
        && config.indent_depth_mm < 0.2
    {
        eprintln!(
            "⚠️  Warning: eyelet indent depth ({:.2}mm) is very shallow",
            config.indent_depth_mm
        );
        eprintln!("   Eyelets may not seat fully — consider increasing indent_depth_mm.");
    }

    let ctx = Ctx {
        ox: outline.bbox.min_x,
        oy: outline.bbox.min_y,
    };

    let board_mp = MultiPolygon::new(vec![outline_to_geo(outline)]);
    let thickness = config.substrate_thickness_mm as f32;
    let chan_depth = config.channel_depth_mm as f32;
    let chan_w = config.channel_width_mm;
    let hole_r = config.pad_hole_diameter_mm / 2.0;

    // Boolean unions for each feature type
    let fcu = union_traces(&pcb.traces_fcu, chan_w);
    let bcu = union_traces(&pcb.traces_bcu, chan_w);

    // Pad holes: only if enabled
    let holes = if config.generate_pad_holes {
        union_circles(&pcb.pads, hole_r, 16)
    } else {
        MultiPolygon::new(vec![])
    };

    // Via indent guides: only if enabled
    let via_indents = if config.generate_via_indents {
        let via_r = config.eyelet_diameter_mm / 2.0;
        union_circles(&pcb.vias.iter().map(|v| Pad { center: v.center, drill: config.eyelet_diameter_mm }).collect::<Vec<_>>(), via_r, 16)
    } else {
        MultiPolygon::new(vec![])
    };

    // ── Top face (z = thickness, normal +Z) ────────────────────────────────
    // Board outline minus F.Cu channel openings minus pad holes minus via indents
    let top_face = board_mp.difference(&fcu).difference(&holes).difference(&via_indents);
    add_flat(&mut mesh, &top_face, &ctx, thickness, true);

    // ── Bottom face (z = 0, normal −Z) ─────────────────────────────────────
    let bot_face = board_mp.difference(&bcu).difference(&holes).difference(&via_indents);
    add_flat(&mut mesh, &bot_face, &ctx, 0.0, false);

    // ── Side walls (z = 0 → thickness) ─────────────────────────────────────
    add_outline_walls(&mut mesh, outline, &ctx, 0.0, thickness);

    // ── F.Cu channel floors + inner walls ──────────────────────────────────
    // Clip channels to board outline, then punch out pad holes
    let fcu_clip = fcu.intersection(&board_mp).difference(&holes);
    add_channel(&mut mesh, &fcu_clip, &ctx, thickness - chan_depth, thickness, true);

    // ── B.Cu channel floors + inner walls ──────────────────────────────────
    let bcu_clip = bcu.intersection(&board_mp).difference(&holes);
    add_channel(&mut mesh, &bcu_clip, &ctx, chan_depth, 0.0, false);

    // ── Pad through-hole cylinder walls ────────────────────────────────────
    if config.generate_pad_holes {
        // Only include pads that are inside the board outline
        let outline_polygon = &outline_to_geo(outline);
        let pads_inside: Vec<_> = pcb
            .pads
            .iter()
            .filter(|pad| {
                use geo::Contains;
                outline_polygon.contains(&Coord { x: pad.center.x, y: pad.center.y })
            })
            .collect();
        add_pad_walls(&mut mesh, &pads_inside, hole_r, &ctx, 0.0, thickness, 16);
    }

    // ── Via eyelet indent guides ──────────────────────────────────────────
    if config.generate_via_indents && !pcb.vias.is_empty() {
        let via_r = config.eyelet_diameter_mm / 2.0;
        let indent_d = config.indent_depth_mm as f32;
        // Top indent dimples
        add_via_indents(&mut mesh, &pcb.vias, via_r, &ctx, thickness, indent_d, 16);
        // Bottom indent dimples
        add_via_indents(&mut mesh, &pcb.vias, via_r, &ctx, 0.0, indent_d, 16);
    }

    Ok(mesh)
}

// ---------------------------------------------------------------------------
// Polygon construction helpers
// ---------------------------------------------------------------------------

fn outline_to_geo(outline: &BoardOutline) -> Polygon {
    let coords: Vec<Coord> = outline
        .vertices
        .iter()
        .map(|p| Coord { x: p.x, y: p.y })
        .collect();
    Polygon::new(LineString::new(coords), vec![])
}

fn trace_rect(trace: &Trace, width: f64) -> Option<Polygon> {
    let dx = trace.end.x - trace.start.x;
    let dy = trace.end.y - trace.start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-10 {
        return None;
    }
    // Perpendicular half-vector
    let px = -dy / len * width / 2.0;
    let py = dx / len * width / 2.0;
    let (sx, sy, ex, ey) = (trace.start.x, trace.start.y, trace.end.x, trace.end.y);
    // CCW winding: start+perp, start-perp, end-perp, end+perp
    Some(Polygon::new(
        LineString::new(vec![
            Coord { x: sx + px, y: sy + py },
            Coord { x: sx - px, y: sy - py },
            Coord { x: ex - px, y: ey - py },
            Coord { x: ex + px, y: ey + py },
            Coord { x: sx + px, y: sy + py },
        ]),
        vec![],
    ))
}

fn circle_poly(center: &Point2, radius: f64, sides: usize) -> Polygon {
    use std::f64::consts::PI;
    let coords: Vec<Coord> = (0..=sides)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / sides as f64;
            Coord {
                x: center.x + radius * a.cos(),
                y: center.y + radius * a.sin(),
            }
        })
        .collect();
    Polygon::new(LineString::new(coords), vec![])
}

fn union_polys(polys: Vec<Polygon>) -> MultiPolygon {
    let valid: Vec<Polygon> = polys
        .into_iter()
        .filter(|p| p.exterior().coords().count() >= 4)
        .collect();
    if valid.is_empty() {
        return MultiPolygon::new(vec![]);
    }
    let mut result = MultiPolygon::new(vec![valid[0].clone()]);
    for poly in &valid[1..] {
        result = result.union(&MultiPolygon::new(vec![poly.clone()]));
    }
    result
}

fn union_traces(traces: &[Trace], channel_width: f64) -> MultiPolygon {
    union_polys(
        traces
            .iter()
            .filter_map(|t| trace_rect(t, channel_width))
            .collect(),
    )
}

fn union_circles(pads: &[Pad], radius: f64, sides: usize) -> MultiPolygon {
    union_polys(pads.iter().map(|p| circle_poly(&p.center, radius, sides)).collect())
}

// ---------------------------------------------------------------------------
// Mesh face generators
// ---------------------------------------------------------------------------

/// Triangulate a MultiPolygon and lift it to height `z`.
/// `normal_up = true`  → CCW triangles (normal +Z).
/// `normal_up = false` → reversed (normal −Z).
fn add_flat(mesh: &mut Mesh3D, mp: &MultiPolygon, ctx: &Ctx, z: f32, normal_up: bool) {
    for poly in mp.iter() {
        for [c0, c1, c2] in triangulate_polygon(poly) {
            let v0 = ctx.coord(&c0, z);
            let v1 = ctx.coord(&c1, z);
            let v2 = ctx.coord(&c2, z);
            if normal_up {
                mesh.tri(v0, v1, v2);
            } else {
                mesh.tri(v0, v2, v1);
            }
        }
    }
}

/// Triangulate a polygon (with possible holes) using the earcut algorithm.
/// Returns a list of triangles as [Coord; 3] arrays.
fn triangulate_polygon(poly: &Polygon) -> Vec<[Coord; 3]> {
    let mut verts: Vec<f64> = Vec::new();
    let mut hole_indices: Vec<usize> = Vec::new();

    push_ring(poly.exterior(), &mut verts);

    for interior in poly.interiors() {
        hole_indices.push(verts.len() / 2);
        push_ring(interior, &mut verts);
    }

    let indices = earcutr::earcut(&verts, &hole_indices, 2).unwrap_or_default();
    let coord_at = |i: usize| Coord { x: verts[i * 2], y: verts[i * 2 + 1] };

    indices
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| [coord_at(c[0]), coord_at(c[1]), coord_at(c[2])])
        .collect()
}

/// Vertical quads along the board outline perimeter.
/// For a CCW exterior ring, this produces outward-facing normals.
fn add_outline_walls(mesh: &mut Mesh3D, outline: &BoardOutline, ctx: &Ctx, z0: f32, z1: f32) {
    let v = &outline.vertices;
    let n = v.len();
    for i in 0..n {
        let a = &v[i];
        let b = &v[(i + 1) % n];
        let a0 = ctx.point(a, z0);
        let b0 = ctx.point(b, z0);
        let b1 = ctx.point(b, z1);
        let a1 = ctx.point(a, z1);
        // Right-of-travel for CCW ring = outward
        mesh.tri(a0, b0, b1);
        mesh.tri(a0, b1, a1);
    }
}

/// Channel floor at `z_floor` + vertical inner walls from `z_floor` to `z_opening`.
/// `is_top = true`  → F.Cu channel (floor normal +Z, walls face inward).
/// `is_top = false` → B.Cu channel (floor normal −Z, walls face inward).
fn add_channel(
    mesh: &mut Mesh3D,
    mp: &MultiPolygon,
    ctx: &Ctx,
    z_floor: f32,
    z_opening: f32,
    is_top: bool,
) {
    // Floor faces
    add_flat(mesh, mp, ctx, z_floor, is_top);

    // Inner walls for every polygon in the union
    for poly in mp.iter() {
        add_ring_walls(mesh, poly.exterior().coords(), z_floor, z_opening, true, ctx);
        for interior in poly.interiors() {
            add_ring_walls(mesh, interior.coords(), z_floor, z_opening, false, ctx);
        }
    }
}

/// Vertical quads along a coordinate ring.
/// `exterior = true`  → inward-facing normals  (CCW ring, normals point left-of-travel).
/// `exterior = false` → outward-facing normals (interior ring, reversed winding).
fn add_ring_walls<'a>(
    mesh: &mut Mesh3D,
    coords_iter: impl Iterator<Item = &'a Coord>,
    z_floor: f32,
    z_opening: f32,
    exterior: bool,
    ctx: &Ctx,
) {
    let coords: Vec<Coord> = coords_iter.copied().collect();
    let n = coords.len();
    if n < 2 {
        return;
    }
    // Skip repeated closing vertex if present
    let n = if coords.first() == coords.last() { n - 1 } else { n };
    if n < 2 {
        return;
    }

    for i in 0..n {
        let a = &coords[i];
        let b = &coords[(i + 1) % n];
        let af = ctx.coord(a, z_floor);
        let bf = ctx.coord(b, z_floor);
        let bo = ctx.coord(b, z_opening);
        let ao = ctx.coord(a, z_opening);

        if exterior {
            // Inward normals: left-of-travel for CCW ring
            mesh.tri(af, bo, bf);
            mesh.tri(af, ao, bo);
        } else {
            // Outward normals: right-of-travel (reversed)
            mesh.tri(af, bf, bo);
            mesh.tri(af, bo, ao);
        }
    }
}

/// Shallow indent dimples at via locations (guide marks for eyelets).
fn add_via_indents(
    mesh: &mut Mesh3D,
    vias: &[crate::pcb::Via],
    radius: f64,
    ctx: &Ctx,
    z_surface: f32,
    indent_depth: f32,
    sides: usize,
) {
    use std::f64::consts::PI;
    let z_floor = z_surface - indent_depth;

    for via in vias {
        let cx = via.center.x;
        let cy = via.center.y;
        for i in 0..sides {
            let a0 = 2.0 * PI * i as f64 / sides as f64;
            let a1 = 2.0 * PI * (i + 1) as f64 / sides as f64;
            let x0 = cx + radius * a0.cos();
            let y0 = cy + radius * a0.sin();
            let x1 = cx + radius * a1.cos();
            let y1 = cy + radius * a1.sin();
            let p00 = ctx.v(x0, y0, z_surface);
            let p10 = ctx.v(x1, y1, z_surface);
            let pf1 = ctx.v(x1, y1, z_floor);
            let pf0 = ctx.v(x0, y0, z_floor);
            mesh.tri(p00, p10, pf0);
            mesh.tri(p10, pf1, pf0);
        }
    }
}

fn push_ring(ring: &geo::LineString, verts: &mut Vec<f64>) {
    let coords: Vec<_> = ring.coords().collect();
    let n = if coords.len() > 1 && coords.first() == coords.last() {
        coords.len() - 1
    } else {
        coords.len()
    };
    for c in &coords[..n] {
        verts.push(c.x);
        verts.push(c.y);
    }
}

/// Cylinder walls for each pad through-hole (inward-facing normals).
fn add_pad_walls(
    mesh: &mut Mesh3D,
    pads: &[&Pad],
    radius: f64,
    ctx: &Ctx,
    z0: f32,
    z1: f32,
    sides: usize,
) {
    use std::f64::consts::PI;
    for pad in pads {
        for i in 0..sides {
            let a0 = 2.0 * PI * i as f64 / sides as f64;
            let a1 = 2.0 * PI * (i + 1) as f64 / sides as f64;
            let x0 = pad.center.x + radius * a0.cos();
            let y0 = pad.center.y + radius * a0.sin();
            let x1 = pad.center.x + radius * a1.cos();
            let y1 = pad.center.y + radius * a1.sin();
            let p00 = ctx.v(x0, y0, z0);
            let p10 = ctx.v(x1, y1, z0);
            let p11 = ctx.v(x1, y1, z1);
            let p01 = ctx.v(x0, y0, z1);
            // CCW around hole → inward
            mesh.tri(p00, p11, p10);
            mesh.tri(p00, p01, p11);
        }
    }
}
