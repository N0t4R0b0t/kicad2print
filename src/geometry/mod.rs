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
// No external clipper fallback available; use guarded geo unions.

use crate::config::Config;
use crate::pcb::{BoardOutline, CutoutShape, Pad, PcbData, Point2, Trace};

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

    // Pad holes: each pad uses its own drill size from KiCad.
    // hole_r serves as a minimum (in case a pad has a tiny or missing drill value).
    let holes = if config.generate_pad_holes {
        union_pad_holes(&pcb.pads, hole_r, 16)
    } else {
        MultiPolygon::new(vec![])
    };

    // Via holes: always treat as through-holes (merged into pad holes polygon)
    let via_holes = if !pcb.vias.is_empty() {
        union_circles(
            &pcb.vias.iter().map(|v| Pad { center: v.center, drill: config.eyelet_diameter_mm, number: String::new(), net_name: None }).collect::<Vec<_>>(),
            config.eyelet_diameter_mm / 2.0,
            16,
        )
    } else {
        MultiPolygon::new(vec![])
    };
    let all_holes = if config.generate_pad_holes {
        holes.union(&via_holes)
    } else {
        via_holes
    };

    // Board cutout holes from Edge.Cuts (fp_rect, gr_rect, gr_circle, etc.)
    let cutouts_mp = if !pcb.cutouts.is_empty() {
        union_cutouts(&pcb.cutouts)
    } else {
        MultiPolygon::new(vec![])
    };
    let all_holes = if !pcb.cutouts.is_empty() {
        all_holes.union(&cutouts_mp)
    } else {
        all_holes
    };

    // ── Generate solid substrate: full board outline minus all through-holes ─
    let solid_substrate = board_mp.difference(&all_holes);

    // ── Top face (z = thickness, normal +Z) ────────────────────────────────
    let top_face = solid_substrate.difference(&fcu);
    add_flat(&mut mesh, &top_face, &ctx, thickness, true);

    // ── Bottom face (z = 0, normal −Z) ─────────────────────────────────────
    let bot_face = if pcb.traces_bcu.is_empty() {
        solid_substrate.clone()
    } else {
        solid_substrate.difference(&bcu)
    };
    add_flat(&mut mesh, &bot_face, &ctx, 0.0, false);

    // ── Side walls (z = 0 → thickness) ─────────────────────────────────────
    add_outline_walls(&mut mesh, outline, &ctx, 0.0, thickness);

    // ── F.Cu channel floors + inner walls ──────────────────────────────────
    let fcu_clip = fcu.intersection(&board_mp).difference(&all_holes);
    add_channel(&mut mesh, &fcu_clip, &ctx, thickness - chan_depth, thickness, true);

    // ── B.Cu channel floors + inner walls ──────────────────────────────────
    if !pcb.traces_bcu.is_empty() {
        let bcu_clip = bcu.intersection(&board_mp).difference(&all_holes);
        add_channel(&mut mesh, &bcu_clip, &ctx, chan_depth, 0.0, false);
    }

    // ── Through-hole cylinder walls (pads + vias) ──────────────────────────
    // Walk the all_holes polygon rings directly — exact same vertices as the face holes.
    for poly in all_holes.iter() {
        add_ring_walls(&mut mesh, poly.exterior().coords(), 0.0, thickness, false, &ctx);
        for interior in poly.interiors() {
            add_ring_walls(&mut mesh, interior.coords(), 0.0, thickness, true, &ctx);
        }
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

/// Build a stadium/capsule polygon for a trace segment: a rectangle with
/// semicircular end caps. This eliminates jagged notches at trace corners
/// when segments are unioned together.
fn trace_rect(trace: &Trace, width: f64) -> Option<Polygon> {
    use std::f64::consts::PI;
    let dx = trace.end.x - trace.start.x;
    let dy = trace.end.y - trace.start.y;
    let len = (dx * dx + dy * dy).sqrt();
    let r = width / 2.0;
    if len < 1e-10 {
        // Degenerate zero-length trace: emit a circle
        return Some(circle_poly(&trace.start, r, 16));
    }

    let ux = dx / len; // forward unit vector
    let uy = dy / len;
    // Left normal (CCW convention)
    let nx = -uy;
    let ny = ux;

    let cap_sides = 8usize; // points per semicircle
    let mut coords: Vec<Coord> = Vec::with_capacity(cap_sides * 2 + 4);

    // CCW capsule:
    // 1. End cap at trace.end: sweep from +normal to -normal going "right" (CW around center)
    //    angle from (fwd+90°) down to (fwd-90°), i.e. from perp to -perp decreasing
    let perp_angle = f64::atan2(ny, nx); // angle of +normal = fwd + PI/2
    for i in 0..=cap_sides {
        let a = perp_angle - PI * i as f64 / cap_sides as f64;
        coords.push(Coord { x: trace.end.x + r * a.cos(), y: trace.end.y + r * a.sin() });
    }

    // 2. Start cap at trace.start: sweep from -normal to +normal going "right" (CW around center)
    //    angle from (fwd-90°) = perp_angle-PI down to (fwd-270°) = perp_angle-2PI
    let neg_perp = perp_angle - PI; // angle of -normal
    for i in 0..=cap_sides {
        let a = neg_perp - PI * i as f64 / cap_sides as f64;
        coords.push(Coord { x: trace.start.x + r * a.cos(), y: trace.start.y + r * a.sin() });
    }

    // Close the ring
    coords.push(coords[0]);

    Some(Polygon::new(LineString::new(coords), vec![]))
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
    // Filter out trivially invalid rings and sanitize coordinates to avoid
    // feeding the boolean-op implementation pathological inputs that can
    // cause internal panics (seen in geo::algorithm::sweep).
    fn clean_polygon(p: Polygon) -> Option<Polygon> {
        let coords: Vec<Coord> = p
            .exterior()
            .coords()
            .map(|c| c.clone())
            .collect();
        if coords.len() < 4 {
            return None;
        }
        // Remove consecutive duplicate points
        let mut cleaned: Vec<Coord> = Vec::with_capacity(coords.len());
        for c in coords.into_iter() {
            if cleaned.last().map(|l: &Coord| l.x == c.x && l.y == c.y).unwrap_or(false) {
                continue;
            }
            cleaned.push(c);
        }
        // Ensure ring is closed
        if cleaned.first() != cleaned.last() {
            if let Some(first) = cleaned.first().cloned() {
                cleaned.push(first);
            }
        }
        if cleaned.len() < 4 {
            return None;
        }
        Some(Polygon::new(LineString::new(cleaned), vec![]))
    }

    let valid: Vec<Polygon> = polys.into_iter().filter_map(clean_polygon).collect();
    if valid.is_empty() {
        return MultiPolygon::new(vec![]);
    }
    // Perform unions incrementally, guarding each union call so a single
    // problematic polygon won't crash the entire process.
    let mut result = MultiPolygon::new(vec![valid[0].clone()]);
    for (i, poly) in valid.iter().enumerate().skip(1) {
        eprintln!("geometry: union iteration {} (poly vertices={})", i, poly.exterior().coords().count());
        let rhs = MultiPolygon::new(vec![poly.clone()]);
        let union_res = std::panic::catch_unwind(|| result.union(&rhs));
        match union_res {
            Ok(mp) => result = mp,
            Err(_) => {
                eprintln!("⚠️  geometry: skipping polygon at index {} that caused boolean-op panic", i);
            }
        }
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

/// Union of pad hole circles, using each pad's own drill diameter (from KiCad).
/// `min_radius` is a floor in case a pad has a missing or unrealistically small drill.
fn union_pad_holes(pads: &[Pad], min_radius: f64, sides: usize) -> MultiPolygon {
    union_polys(
        pads.iter()
            .map(|p| {
                let r = (p.drill / 2.0).max(min_radius);
                circle_poly(&p.center, r, sides)
            })
            .collect(),
    )
}

/// Converts a list of `CutoutShape` items to a union `MultiPolygon`.
fn union_cutouts(cutouts: &[CutoutShape]) -> MultiPolygon {
    union_polys(
        cutouts.iter()
            .map(|c| match *c {
                CutoutShape::Rect { cx, cy, hw, hh, rot } => rect_cutout_poly(cx, cy, hw, hh, rot),
                CutoutShape::Circle { cx, cy, r } => circle_poly(&Point2::new(cx, cy), r, 32),
            })
            .collect(),
    )
}

/// Generates a (possibly rotated) rectangle polygon in geo coordinates.
fn rect_cutout_poly(cx: f64, cy: f64, hw: f64, hh: f64, rot_deg: f64) -> Polygon {
    let rot = rot_deg.to_radians();
    let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let coords: Vec<Coord<f64>> = corners
        .iter()
        .map(|&(lx, ly)| {
            // Rotate corner around center then translate
            let gx = cx + lx * rot.cos() - ly * rot.sin();
            let gy = cy + lx * rot.sin() + ly * rot.cos();
            Coord { x: gx, y: gy }
        })
        .collect();
    Polygon::new(LineString::new(coords), vec![])
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
        // For top channels (is_top=true): groove opens upward, walls need outward-facing normals
        // For bottom channels (is_top=false): groove opens downward, walls need inward-facing normals
        add_ring_walls(mesh, poly.exterior().coords(), z_floor, z_opening, !is_top, ctx);
        for interior in poly.interiors() {
            add_ring_walls(mesh, interior.coords(), z_floor, z_opening, is_top, ctx);
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
/// All vias use eyelet_diameter_mm for consistent sizing.
/// Generate via dimple geometry from the via_indents polygon.
/// The face polygon already has holes with these exact ring vertices.
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

