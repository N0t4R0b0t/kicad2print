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

use crate::config::{Config, StencilMount};
use crate::pcb::{BoardOutline, CopperLayer, CutoutShape, Pad, PadShape, PcbData, Point2, Trace};

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

    // Pad lands: shallow, pad-shaped indents (same depth as trace channels) so
    // electroplating fills a properly shaped, solderable pad — not just the
    // lead's round drill hole. Merged into each layer's channel network so a
    // trace flows continuously into its pad. THT pads still get a real
    // through-hole for the lead (below) cut through the middle of this indent.
    let (fcu, bcu) = if config.generate_pad_lands {
        let pad_lands_fcu = union_pad_lands(&pcb.pads, 0.0, |p| p.on_fcu);
        let pad_lands_bcu = union_pad_lands(&pcb.pads, 0.0, |p| p.on_bcu);
        (
            safe_union(fcu, &pad_lands_fcu, "F.Cu pad lands"),
            safe_union(bcu, &pad_lands_bcu, "B.Cu pad lands"),
        )
    } else {
        (fcu, bcu)
    };

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
            &pcb.vias.iter().map(|v| Pad {
                center: v.center,
                drill: config.eyelet_diameter_mm,
                number: String::new(),
                net_name: None,
                width: config.eyelet_diameter_mm,
                height: config.eyelet_diameter_mm,
                shape: PadShape::Circle,
                rotation_deg: 0.0,
                on_fcu: true,
                on_bcu: true,
            }).collect::<Vec<_>>(),
            config.eyelet_diameter_mm / 2.0,
            16,
        )
    } else {
        MultiPolygon::new(vec![])
    };
    let all_holes = if config.generate_pad_holes {
        safe_union(holes, &via_holes, "pad holes + via holes")
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
        safe_union(all_holes, &cutouts_mp, "holes + cutouts")
    } else {
        all_holes
    };

    // ── Generate solid substrate: full board outline minus all through-holes ─
    let solid_substrate = safe_difference(board_mp.clone(), &all_holes, "board outline - all holes");

    // Clip the channel/pad-land networks against the board outline and all
    // holes (drills + cutouts) exactly once, up front, and reuse this single
    // clipped version everywhere below. Previously `top_face`/`bot_face` were
    // cut using the *unclipped* fcu/bcu (still extending past cutout
    // boundaries), while the channel-floor code separately computed its own
    // clipped copy — two independently-clipped versions of the same feature
    // meeting at nearly-but-not-exactly-coincident edges is exactly the kind
    // of degenerate near-touching geometry that the underlying `geo` crate's
    // boolean-op sweep algorithm can silently mis-triangulate (confirmed:
    // reproducibly corrupted the region around a footprint that has a real
    // Edge.Cuts cutout overlapping its own pads/trace stubs). A single
    // consistently-clipped fcu/bcu avoids feeding that same boundary into the
    // sweep algorithm twice from two different starting shapes.
    let fcu = safe_intersection(fcu, &board_mp, "F.Cu network ∩ board outline");
    let fcu = safe_difference(fcu, &all_holes, "F.Cu network - all holes");
    let bcu = safe_intersection(bcu, &board_mp, "B.Cu network ∩ board outline");
    let bcu = safe_difference(bcu, &all_holes, "B.Cu network - all holes");

    // ── Top face (z = thickness, normal +Z) ────────────────────────────────
    let top_face = safe_difference(solid_substrate.clone(), &fcu, "top face - F.Cu network");
    add_flat(&mut mesh, &top_face, &ctx, thickness, true);

    // ── Bottom face (z = 0, normal −Z) ─────────────────────────────────────
    let bot_face = if pcb.traces_bcu.is_empty() {
        solid_substrate.clone()
    } else {
        safe_difference(solid_substrate.clone(), &bcu, "bottom face - B.Cu network")
    };
    add_flat(&mut mesh, &bot_face, &ctx, 0.0, false);

    // ── Side walls (z = 0 → thickness) ─────────────────────────────────────
    add_outline_walls(&mut mesh, outline, &ctx, 0.0, thickness);

    // ── F.Cu channel floors + inner walls ──────────────────────────────────
    add_channel(&mut mesh, &fcu, &ctx, thickness - chan_depth, thickness, true);

    // ── B.Cu channel floors + inner walls ──────────────────────────────────
    if !pcb.traces_bcu.is_empty() {
        add_channel(&mut mesh, &bcu, &ctx, chan_depth, 0.0, false);
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
    // problematic polygon won't crash the entire process. The geo crate's
    // boolean-op sweep algorithm has been observed to both panic AND hang
    // indefinitely on pathological/degenerate input (e.g. capsule polygons
    // that touch at an exact shared vertex, as adjacent PCB trace segments
    // do) — catch_unwind alone can't stop a hang, so each union also runs
    // under a watchdog timeout on a background thread.
    let mut result = MultiPolygon::new(vec![valid[0].clone()]);
    for (i, poly) in valid.iter().enumerate().skip(1) {
        let rhs = MultiPolygon::new(vec![poly.clone()]);
        match geo_op_with_timeout(result.clone(), rhs, GEO_OP_TIMEOUT, geo_union) {
            Some(mp) => result = mp,
            None => {
                eprintln!("⚠️  geometry: skipping polygon at index {} that caused a boolean-op panic or timeout", i);
            }
        }
    }
    result
}

/// Runs a `geo` boolean op (union/intersection/difference) on a background
/// thread and gives up after `timeout`, returning `None` on panic or timeout
/// instead of hanging or crashing the whole process. The abandoned thread (if
/// any) is left to run and is killed with the process on exit — this is a
/// short-lived CLI tool, so leaking one stuck thread per failed op is an
/// acceptable trade for never hanging or aborting.
fn geo_op_with_timeout(
    lhs: MultiPolygon,
    rhs: MultiPolygon,
    timeout: std::time::Duration,
    op: fn(&MultiPolygon, &MultiPolygon) -> MultiPolygon,
) -> Option<MultiPolygon> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| op(&lhs, &rhs)));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(mp)) => Some(mp),
        Ok(Err(_)) | Err(_) => None,
    }
}

fn geo_union(a: &MultiPolygon, b: &MultiPolygon) -> MultiPolygon { a.union(b) }
fn geo_intersection(a: &MultiPolygon, b: &MultiPolygon) -> MultiPolygon { a.intersection(b) }
fn geo_difference(a: &MultiPolygon, b: &MultiPolygon) -> MultiPolygon { a.difference(b) }

const GEO_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Safe union: on panic/timeout, warns and returns `lhs` unchanged (drops the
/// rhs contribution) rather than crashing or hanging the whole conversion.
fn safe_union(lhs: MultiPolygon, rhs: &MultiPolygon, what: &str) -> MultiPolygon {
    match geo_op_with_timeout(lhs.clone(), rhs.clone(), GEO_OP_TIMEOUT, geo_union) {
        Some(mp) => mp,
        None => {
            eprintln!("⚠️  geometry: union failed ({what}) — keeping prior geometry, dropping this contribution");
            lhs
        }
    }
}

/// Safe intersection: on panic/timeout, warns and returns `lhs` unchanged
/// (skips clipping) rather than crashing or hanging the whole conversion.
fn safe_intersection(lhs: MultiPolygon, rhs: &MultiPolygon, what: &str) -> MultiPolygon {
    match geo_op_with_timeout(lhs.clone(), rhs.clone(), GEO_OP_TIMEOUT, geo_intersection) {
        Some(mp) => mp,
        None => {
            eprintln!("⚠️  geometry: intersection failed ({what}) — keeping prior geometry, skipping this clip");
            lhs
        }
    }
}

/// Safe difference: on panic/timeout, warns and returns `lhs` unchanged
/// (skips subtracting rhs) rather than crashing or hanging the whole
/// conversion. Note: unlike a failed union/intersection, a failed difference
/// leaves `rhs`'s area un-subtracted from `lhs` — for a hole cut this means
/// the hole may be missing in the small, rare case this triggers, which is a
/// far better failure mode than an aborted process or a corrupted mesh.
fn safe_difference(lhs: MultiPolygon, rhs: &MultiPolygon, what: &str) -> MultiPolygon {
    match geo_op_with_timeout(lhs.clone(), rhs.clone(), GEO_OP_TIMEOUT, geo_difference) {
        Some(mp) => mp,
        None => {
            eprintln!("⚠️  geometry: difference failed ({what}) — keeping prior geometry, skipping this cut");
            lhs
        }
    }
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
/// `min_radius` is a floor applied only to pads that already have a real KiCad
/// drill (guards against a tiny/degenerate drill value) — a pad with no drill
/// at all (`drill == 0.0`, e.g. an SMD pad) is skipped entirely and never gets
/// a fabricated hole; it may still get a shaped land indent (`pad_land_poly`).
fn union_pad_holes(pads: &[Pad], min_radius: f64, sides: usize) -> MultiPolygon {
    union_polys(
        pads.iter()
            .filter(|p| p.drill > 0.0)
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

/// Builds a stadium (rect with semicircular caps) for a KiCad "oval" pad of
/// local size w×h, rotated by `rot_deg` and centered at (cx, cy). Degenerates
/// to a plain circle when w == h. `cap_sides` is the number of segments per
/// semicircular cap.
fn oval_poly(cx: f64, cy: f64, w: f64, h: f64, rot_deg: f64, cap_sides: usize) -> Polygon {
    use std::f64::consts::PI;
    let rot = rot_deg.to_radians();
    let to_global = |lx: f64, ly: f64| Coord {
        x: cx + lx * rot.cos() - ly * rot.sin(),
        y: cy + lx * rot.sin() + ly * rot.cos(),
    };

    if (w - h).abs() < 1e-9 {
        // Square aspect ratio — a plain circle.
        let r = w / 2.0;
        let coords: Vec<Coord> = (0..=cap_sides * 2)
            .map(|i| {
                let a = 2.0 * PI * i as f64 / (cap_sides * 2) as f64;
                to_global(r * a.cos(), r * a.sin())
            })
            .collect();
        return Polygon::new(LineString::new(coords), vec![]);
    }

    // Local-frame stadium: long axis picked by whichever of w/h is larger.
    let r = w.min(h) / 2.0;
    let mut coords: Vec<Coord> = Vec::with_capacity(cap_sides * 2 + 2);
    if w >= h {
        let half_straight = (w - h) / 2.0;
        // Cap centered at +half_straight, sweeping the right semicircle (-90°..+90°)
        for i in 0..=cap_sides {
            let a = -PI / 2.0 + PI * i as f64 / cap_sides as f64;
            coords.push(to_global(half_straight + r * a.cos(), r * a.sin()));
        }
        // Cap centered at -half_straight, sweeping the left semicircle (90°..270°)
        for i in 0..=cap_sides {
            let a = PI / 2.0 + PI * i as f64 / cap_sides as f64;
            coords.push(to_global(-half_straight + r * a.cos(), r * a.sin()));
        }
    } else {
        let half_straight = (h - w) / 2.0;
        // Cap centered at +half_straight (top), sweeping (0°..180°)
        for i in 0..=cap_sides {
            let a = PI * i as f64 / cap_sides as f64;
            coords.push(to_global(r * a.cos(), half_straight + r * a.sin()));
        }
        // Cap centered at -half_straight (bottom), sweeping (180°..360°)
        for i in 0..=cap_sides {
            let a = PI + PI * i as f64 / cap_sides as f64;
            coords.push(to_global(r * a.cos(), -half_straight + r * a.sin()));
        }
    }
    coords.push(coords[0]);
    Polygon::new(LineString::new(coords), vec![])
}

/// Builds the copper land polygon for a pad, in its real shape/size/orientation
/// (rect, rounded-rect approximated as rect, circle, or oval/stadium) — used to
/// carve an accurately-shaped indent/slot rather than a round hole. Returns
/// `None` for a pad with no usable size (shouldn't normally happen).
/// `margin_mm` inflates width/height symmetrically (e.g. to widen a stencil
/// opening past the substrate's exact pad size for paint/alignment tolerance,
/// matching how trace slots get `stencil_slot_clearance_mm` — pass 0.0 for an
/// exact-size land).
fn pad_land_poly(pad: &Pad, margin_mm: f64) -> Option<Polygon> {
    if pad.width <= 0.0 || pad.height <= 0.0 {
        return None;
    }
    let w = pad.width + margin_mm;
    let h = pad.height + margin_mm;
    Some(match pad.shape {
        PadShape::Rect | PadShape::RoundRect => {
            rect_cutout_poly(pad.center.x, pad.center.y, w / 2.0, h / 2.0, pad.rotation_deg)
        }
        PadShape::Circle => circle_poly(&pad.center, w.max(h) / 2.0, 24),
        PadShape::Oval => oval_poly(pad.center.x, pad.center.y, w, h, pad.rotation_deg, 12),
    })
}

/// Union of pad land shapes (see `pad_land_poly`) for pads matching `filter`
/// (typically an on_fcu/on_bcu check for the layer being built).
fn union_pad_lands(pads: &[Pad], margin_mm: f64, filter: impl Fn(&Pad) -> bool) -> MultiPolygon {
    union_polys(
        pads.iter()
            .filter(|p| filter(p))
            .filter_map(|p| pad_land_poly(p, margin_mm))
            .collect(),
    )
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
///
/// `earcut` assumes a simple (non-self-intersecting), duplicate-free ring.
/// Rings coming out of several chained `geo` boolean ops can carry
/// sub-micron floating-point noise — near-duplicate or near-collinear
/// vertices that are mathematically harmless but make earcut's ear-clipping
/// produce scattered, fragmented garbage instead of the intended shape
/// (confirmed: a real pad land recessed floor came out as disconnected
/// slivers instead of a clean rectangle). Snapping to a fixed grid right
/// before triangulation — well below any manufacturing tolerance — resolves
/// this without needing to fix the noise at its various upstream sources.
fn triangulate_polygon(poly: &Polygon) -> Vec<[Coord; 3]> {
    let mut verts: Vec<f64> = Vec::new();
    let mut hole_indices: Vec<usize> = Vec::new();

    push_ring_snapped(poly.exterior(), &mut verts);

    for interior in poly.interiors() {
        hole_indices.push(verts.len() / 2);
        push_ring_snapped(interior, &mut verts);
    }

    let indices = earcutr::earcut(&verts, &hole_indices, 2).unwrap_or_default();
    let coord_at = |i: usize| Coord { x: verts[i * 2], y: verts[i * 2 + 1] };

    indices
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| [coord_at(c[0]), coord_at(c[1]), coord_at(c[2])])
        .collect()
}

/// 1/10000 mm = 0.1 micron — far finer than any FDM/resin printer can
/// resolve, but coarse enough to collapse the floating-point noise that
/// chained boolean ops leave behind onto exactly-coincident coordinates.
fn snap_coord(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Like `push_ring`, but snaps coordinates to `snap_coord` and drops
/// consecutive duplicate/near-zero-length edges the snap can introduce —
/// earcut degrades badly on both.
fn push_ring_snapped(ring: &geo::LineString, verts: &mut Vec<f64>) {
    let coords: Vec<_> = ring.coords().collect();
    let n = if coords.len() > 1 && coords.first() == coords.last() {
        coords.len() - 1
    } else {
        coords.len()
    };
    let ring_start = verts.len();
    let mut last: Option<(f64, f64)> = None;
    for c in &coords[..n] {
        let (x, y) = (snap_coord(c.x), snap_coord(c.y));
        if last == Some((x, y)) {
            continue;
        }
        verts.push(x);
        verts.push(y);
        last = Some((x, y));
    }
    // Drop a trailing point that snapped onto *this ring's own* first point
    // (not any earlier ring already in `verts` — this function is called once
    // per ring, exterior then each hole, all sharing one accumulator).
    if verts.len() >= ring_start + 4 {
        let (fx, fy) = (verts[ring_start], verts[ring_start + 1]);
        let (lx, ly) = (verts[verts.len() - 2], verts[verts.len() - 1]);
        if (fx, fy) == (lx, ly) {
            verts.truncate(verts.len() - 2);
        }
    }
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

/// Axis-aligned rectangle as a CCW polygon ring (no holes).
fn rect_poly(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon::new(
        LineString::new(vec![
            Coord { x: x0, y: y0 },
            Coord { x: x1, y: y0 },
            Coord { x: x1, y: y1 },
            Coord { x: x0, y: y1 },
            Coord { x: x0, y: y0 },
        ]),
        vec![],
    )
}

/// Nearest point to `p` on segment `a`→`b` (clamped to the endpoints).
fn nearest_on_segment(p: Point2, a: Point2, b: Point2) -> Point2 {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len2 = abx * abx + aby * aby;
    if len2 < 1e-12 {
        return a;
    }
    let t = (((p.x - a.x) * abx + (p.y - a.y) * aby) / len2).clamp(0.0, 1.0);
    Point2::new(a.x + t * abx, a.y + t * aby)
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

// ---------------------------------------------------------------------------
// Snap-on conductive-paint stencil + temporary plating bus
// ---------------------------------------------------------------------------

/// Generate a snap-on conductive-paint stencil for a single copper layer.
///
/// The stencil is a thin plate that registers over the substrate top via a
/// perimeter snap-lip. Through-slots sit over every trace groove so conductive
/// paint squeegees only into the channels (minimal cleanup). Additional slots
/// form a temporary plating bus — a perimeter rail plus one stub to each
/// electrically-isolated trace island — so the entire layer plates from a
/// single cathode contact. The bus bars sit proud on the flat substrate and are
/// ground off after plating to isolate the traces.
///
/// Trace islands are found purely geometrically: after unioning the layer's
/// traces, each resulting polygon is one electrically-connected island, so no
/// net information is required.
///
/// Returns `Ok(None)` when the layer has no traces.
pub fn generate_stencil(
    pcb: &PcbData,
    config: &Config,
    layer: CopperLayer,
) -> Result<Option<Mesh3D>> {
    let traces = match layer {
        CopperLayer::FCu => &pcb.traces_fcu,
        CopperLayer::BCu => &pcb.traces_bcu,
    };
    if traces.is_empty() {
        return Ok(None);
    }

    let outline = pcb
        .outline
        .as_ref()
        .ok_or_else(|| anyhow!("No board outline found — cannot generate stencil"))?;
    let bbox = &outline.bbox;
    let ctx = Ctx {
        ox: bbox.min_x,
        oy: bbox.min_y,
    };

    let slot_w = config.channel_width_mm + 2.0 * config.stencil_slot_clearance_mm;
    let bus_w = config.bus_width_mm;
    let inset = config.bus_inset_mm;
    let plate_t = config.stencil_thickness_mm as f32;
    let bus = config.stencil_plating_bus;

    // True board region — keeps bus features on the board.
    let board_mp = MultiPolygon::new(vec![outline_to_geo(outline)]);

    // ── Trace slots: each unioned polygon is one isolated copper island ─────
    let trace_slots = union_traces(traces, slot_w);

    // ── Pad lands: pad-shaped (not round) through-slots so paint/plating fills
    // the substrate's matching pad-shaped indent, not just a round lead hole.
    // Widened by the same slot clearance as trace channels for paint/alignment
    // tolerance. Merged into trace_slots so a pad's slot joins its trace's.
    let trace_slots = if config.generate_pad_lands {
        let land_margin = 2.0 * config.stencil_slot_clearance_mm;
        let on_layer = |p: &Pad| match layer {
            CopperLayer::FCu => p.on_fcu,
            CopperLayer::BCu => p.on_bcu,
        };
        let pad_lands = union_pad_lands(&pcb.pads, land_margin, on_layer);
        safe_union(trace_slots, &pad_lands, "stencil trace slots + pad lands")
    } else {
        trace_slots
    };

    // ── Pad + via holes so the plate clears inserted leads/eyelets and lets
    // paint reach the eyelet flanges (mirrors the substrate's through-holes).
    let pad_holes = if config.generate_pad_holes {
        union_pad_holes(&pcb.pads, config.pad_hole_diameter_mm / 2.0, 16)
    } else {
        MultiPolygon::new(vec![])
    };
    let via_holes = if pcb.vias.is_empty() {
        MultiPolygon::new(vec![])
    } else {
        union_circles(
            &pcb.vias
                .iter()
                .map(|v| Pad {
                    center: v.center,
                    drill: config.eyelet_diameter_mm,
                    number: String::new(),
                    net_name: None,
                    width: config.eyelet_diameter_mm,
                    height: config.eyelet_diameter_mm,
                    shape: PadShape::Circle,
                    rotation_deg: 0.0,
                    on_fcu: true,
                    on_bcu: true,
                })
                .collect::<Vec<_>>(),
            config.eyelet_diameter_mm / 2.0,
            16,
        )
    };
    let hole_slots = safe_union(pad_holes, &via_holes, "stencil pad holes + via holes");

    // Rail centerline rectangle — used to route stubs and place tie-bars. On a
    // strongly non-rectangular outline the centerline isn't clipped to the board,
    // so a stub could aim at a clipped span; rectangular boards are unaffected.
    let (rx0, ry0) = (bbox.min_x + inset, bbox.min_y + inset);
    let (rx1, ry1) = (bbox.max_x - inset, bbox.max_y - inset);
    let cx0 = rx0 + bus_w / 2.0;
    let cy0 = ry0 + bus_w / 2.0;
    let cx1 = rx1 - bus_w / 2.0;
    let cy1 = ry1 - bus_w / 2.0;
    let rail_segments = [
        (Point2::new(cx0, cy0), Point2::new(cx1, cy0)),
        (Point2::new(cx1, cy0), Point2::new(cx1, cy1)),
        (Point2::new(cx1, cy1), Point2::new(cx0, cy1)),
        (Point2::new(cx0, cy1), Point2::new(cx0, cy0)),
    ];
    let tie_w = config.bus_tie_width_mm;
    let tie_pad = bus_w.max(1.0);

    // ── Temporary plating bus (optional — `stencil_plating_bus`, off by default)
    // A perimeter rail + one stub per isolated trace shorts every trace to a
    // single cathode contact for electroplating; tie-bars keep the fenced-in
    // plate attached. A plain paint stencil is just the traces and holes above.
    let (bus_slots, tie_mp) = if bus {
        // Perimeter rail ring inset from the bbox.
        let rail_mp = if rx1 - rx0 > 2.5 * bus_w && ry1 - ry0 > 2.5 * bus_w {
            let outer = MultiPolygon::new(vec![rect_poly(rx0, ry0, rx1, ry1)]);
            let inner = MultiPolygon::new(vec![rect_poly(rx0 + bus_w, ry0 + bus_w, rx1 - bus_w, ry1 - bus_w)]);
            let ring = safe_difference(outer, &inner, "stencil bus rail ring");
            safe_intersection(ring, &board_mp, "stencil bus rail ring ∩ board outline")
        } else {
            // Board too small for a ring — a single bus bar along one edge.
            let bar = MultiPolygon::new(vec![rect_poly(rx0, ry0, rx1, ry0 + bus_w)]);
            safe_intersection(bar, &board_mp, "stencil bus bar ∩ board outline")
        };

        // Mid-edge tie-bars hold the plate the rail fences in. Each interrupts the
        // painted rail (→ its own cathode arc); count is size-based / configurable,
        // and further loose bodies are tied on demand by bridge_loose_bodies().
        let board_w = bbox.max_x - bbox.min_x;
        let board_h = bbox.max_y - bbox.min_y;
        let n_ties = if config.bus_tie_count > 0 {
            config.bus_tie_count as usize
        } else if board_w.max(board_h) > 30.0 {
            2
        } else {
            1
        };
        let xmid = (rx0 + rx1) / 2.0;
        let ymid = (ry0 + ry1) / 2.0;
        let candidates: [Point2; 4] = if board_w >= board_h {
            [Point2::new(xmid, cy0), Point2::new(xmid, cy1), Point2::new(cx0, ymid), Point2::new(cx1, ymid)]
        } else {
            [Point2::new(cx0, ymid), Point2::new(cx1, ymid), Point2::new(xmid, cy0), Point2::new(xmid, cy1)]
        };
        let tie_mp = MultiPolygon::new(
            candidates
                .iter()
                .take(n_ties.min(4))
                .map(|t| rail_tie_rect(&rail_segments, bus_w, tie_w, *t, tie_pad))
                .collect(),
        );

        // One stub from each isolated trace island to the nearest rail point.
        let mut stub_polys: Vec<Polygon> = Vec::new();
        for island in trace_slots.iter() {
            let mut best: Option<(f64, Point2, Point2)> = None;
            for c in island.exterior().coords() {
                let p = Point2::new(c.x, c.y);
                for (a, b) in &rail_segments {
                    let q = nearest_on_segment(p, *a, *b);
                    let d = p.distance_to(q);
                    if best.map(|(bd, _, _)| d < bd).unwrap_or(true) {
                        best = Some((d, p, q));
                    }
                }
            }
            if let Some((_, p, q)) = best {
                let stub = Trace { layer, start: p, end: q, width: bus_w };
                if let Some(poly) = trace_rect(&stub, bus_w) {
                    stub_polys.push(poly);
                }
            }
        }
        (safe_union(rail_mp, &union_polys(stub_polys), "stencil bus rail + stubs"), tie_mp)
    } else {
        (MultiPolygon::new(vec![]), MultiPolygon::new(vec![]))
    };

    // All through-slots = traces ∪ holes ∪ (bus, if enabled).
    let slots = safe_union(trace_slots, &hole_slots, "stencil slots + hole slots");
    let slots = safe_union(slots, &bus_slots, "stencil slots + bus slots");

    // ── Plate footprint + slot region (depend on the mount style) ───────────
    let clr = config.stencil_fit_clearance_mm;
    let wt = config.stencil_wall_thickness_mm;
    let (plate_outer, clip_inner) = match config.stencil_mount {
        // Lip: the plate overhangs the board to carry the integral perimeter lip,
        // and slots live within the cavity (bbox + fit clearance).
        StencilMount::Lip => (
            rect_poly(bbox.min_x - clr - wt, bbox.min_y - clr - wt, bbox.max_x + clr + wt, bbox.max_y + clr + wt),
            rect_poly(bbox.min_x - clr, bbox.min_y - clr, bbox.max_x + clr, bbox.max_y + clr),
        ),
        // Ring: a flat, board-sized plate held by a separate clamp ring; slots
        // live within the board footprint.
        StencilMount::Ring => {
            let r = rect_poly(bbox.min_x, bbox.min_y, bbox.max_x, bbox.max_y);
            (r.clone(), r)
        }
    };
    let plate_mp = MultiPolygon::new(vec![plate_outer.clone()]);
    let clip_mp = MultiPolygon::new(vec![clip_inner.clone()]);

    // Clip slots to the plate's slot region. With the bus on, carve the tie-bars
    // and bridge any remaining loose plate bodies across the rail so nothing
    // prints detached; a plain paint stencil (traces + holes) needs neither.
    let slots = safe_intersection(slots, &clip_mp, "stencil slots ∩ clip region");
    let slots = if bus {
        let slots = safe_difference(slots, &tie_mp, "stencil slots - tie bars");
        let top_face = safe_difference(plate_mp.clone(), &slots, "stencil top face - slots (loose-body check)");
        let extra_ties = bridge_loose_bodies(&top_face, &rail_segments, bus_w, tie_w, tie_pad);
        safe_difference(slots, &extra_ties, "stencil slots - extra ties")
    } else {
        slots
    };

    // ── Build the stencil as a single watertight shell ──────────────────────
    // (make_outward_consistent() at the end re-orients the whole shell into one
    // consistent outward manifold, so the slot walls' best-effort winding here is
    // fine — without it a slicer like Cura fills holes whose walls face the wrong
    // way, the "preview shows slots / slice comes out blank" failure.)
    let mut mesh = Mesh3D::default();
    match config.stencil_mount {
        // Integral perimeter lip. Cross-section (one closed manifold):
        //   plate_t ┤  ┌───────────────────────────┐   ← top face (slots punched)
        //        0  ┤  ├──────────┐       ┌─────────┤   ← cavity underside (on board)
        //      −wh  ┤  └──────────┘       └─────────┘   ← lip bottom rim
        StencilMount::Lip => {
            let wh = config.stencil_wall_height_mm as f32;
            let top = safe_difference(plate_mp.clone(), &slots, "stencil lip top face - slots");
            let underside = safe_difference(clip_mp.clone(), &slots, "stencil lip cavity underside - slots");
            let rim = safe_difference(plate_mp.clone(), &clip_mp, "stencil lip bottom rim - clip region");
            add_flat(&mut mesh, &top, &ctx, plate_t, true); // top
            add_flat(&mut mesh, &underside, &ctx, 0.0, false); // cavity underside
            add_flat(&mut mesh, &rim, &ctx, -wh, false); // lip bottom rim
            add_ring_walls(&mut mesh, plate_outer.exterior().coords(), -wh, plate_t, false, &ctx);
            add_ring_walls(&mut mesh, clip_inner.exterior().coords(), -wh, 0.0, true, &ctx);
            add_slot_walls(&mut mesh, &slots, 0.0, plate_t, &ctx);
            // B.Cu lip wraps the opposite way → mirror in Z (keeps slot XY).
            if layer == CopperLayer::BCu {
                for t in mesh.triangles.iter_mut() {
                    for v in t.vertices.iter_mut() {
                        v[2] = -v[2];
                    }
                }
            }
        }
        // Flat slotted plate — no lip, no cavity step. Prints contact-face-down for
        // a smooth masking finish; a separate clamp ring (generate_clamp_ring)
        // registers it. The plate is Z-symmetric, so the B.Cu plate needs no
        // mirror — only the slot XY matters.
        StencilMount::Ring => {
            let faces = safe_difference(plate_mp.clone(), &slots, "stencil ring plate face - slots");
            add_flat(&mut mesh, &faces, &ctx, plate_t, true); // top
            add_flat(&mut mesh, &faces, &ctx, 0.0, false); // contact face
            add_ring_walls(&mut mesh, plate_outer.exterior().coords(), 0.0, plate_t, false, &ctx);
            add_slot_walls(&mut mesh, &slots, 0.0, plate_t, &ctx);
        }
    }

    // Force a single, consistently-outward orientation so the STL slices cleanly.
    make_outward_consistent(&mut mesh);

    Ok(Some(mesh))
}

/// Vertical walls for every through-slot in a plate (top → bottom face). Winding
/// is best-effort; make_outward_consistent() re-orients the final shell.
fn add_slot_walls(mesh: &mut Mesh3D, slots: &MultiPolygon, z_bot: f32, z_top: f32, ctx: &Ctx) {
    for poly in slots.iter() {
        add_ring_walls(mesh, poly.exterior().coords(), z_bot, z_top, false, ctx);
        for interior in poly.interiors() {
            add_ring_walls(mesh, interior.coords(), z_bot, z_top, true, ctx);
        }
    }
}

/// Generate the reusable L-section clamp ring for the `ring` stencil mount.
///
/// A rectangular picture-frame that snaps around the PCB (friction fit on the
/// board edges) and folds a lip inward over the flat stencil plate to wedge it
/// against the board face. Reused per side (flip the board between sides).
/// Cross-section through one edge:
///
///   ring_top ┤  ┌────┐
///            │  │    │  ← lip covers the plate by `overlap`
/// plate_top ┤  │    └┐ ─────────  plate top
///            │  │     │  (plate + PCB sit in the opening)
///        0  ┤  └─────┘ ─────────  PCB bottom
///              │← wt →│← clr →│ board edge
pub fn generate_clamp_ring(pcb: &PcbData, config: &Config) -> Result<Mesh3D> {
    let outline = pcb
        .outline
        .as_ref()
        .ok_or_else(|| anyhow!("No board outline found — cannot generate clamp ring"))?;
    let bbox = &outline.bbox;
    let ctx = Ctx { ox: bbox.min_x, oy: bbox.min_y };

    let clr = config.stencil_fit_clearance_mm;
    let wt = config.stencil_wall_thickness_mm;
    // Keep the lip from crossing the board centre on tiny boards.
    let half_min = ((bbox.max_x - bbox.min_x).min(bbox.max_y - bbox.min_y) / 2.0 - 0.5).max(0.0);
    let overlap = config.ring_lip_overlap_mm.min(half_min);
    let plate_top = (config.substrate_thickness_mm + config.stencil_thickness_mm) as f32;
    let ring_top = plate_top + config.ring_lip_height_mm as f32;

    // outer wall │ inner wall (grips PCB+plate) │ lip overhang over the plate
    let outer = rect_poly(bbox.min_x - clr - wt, bbox.min_y - clr - wt, bbox.max_x + clr + wt, bbox.max_y + clr + wt);
    let inner_wall = rect_poly(bbox.min_x - clr, bbox.min_y - clr, bbox.max_x + clr, bbox.max_y + clr);
    let lip_hole = rect_poly(bbox.min_x + overlap, bbox.min_y + overlap, bbox.max_x - overlap, bbox.max_y - overlap);

    let outer_mp = MultiPolygon::new(vec![outer.clone()]);
    let lower_band = outer_mp.difference(&MultiPolygon::new(vec![inner_wall.clone()])); // vertical wall
    let upper_band = outer_mp.difference(&MultiPolygon::new(vec![lip_hole.clone()])); // wall + inward lip
    let ledge = MultiPolygon::new(vec![inner_wall.clone()])
        .difference(&MultiPolygon::new(vec![lip_hole.clone()])); // lip underside (rests on plate)

    let mut mesh = Mesh3D::default();
    add_flat(&mut mesh, &lower_band, &ctx, 0.0, false); // bottom rim
    add_flat(&mut mesh, &ledge, &ctx, plate_top, false); // lip underside
    add_flat(&mut mesh, &upper_band, &ctx, ring_top, true); // top
    add_ring_walls(&mut mesh, outer.exterior().coords(), 0.0, ring_top, false, &ctx);
    add_ring_walls(&mut mesh, inner_wall.exterior().coords(), 0.0, plate_top, true, &ctx);
    add_ring_walls(&mut mesh, lip_hole.exterior().coords(), plate_top, ring_top, true, &ctx);

    make_outward_consistent(&mut mesh);
    Ok(mesh)
}

/// Find plate regions that print as loose bodies (fully fenced off by slots) and
/// return tie-bar rectangles that bridge each one across the bus rail to the outer
/// frame, so the whole plate survives printing and peeling as a single piece.
///
/// Connectivity is measured on the triangulated mesh (triangle-edge adjacency),
/// not on the geo polygons — geo can report two regions joined only at a pinch
/// point as a single polygon, but a pinch has no real strength and prints loose.
///
/// Only loose bodies that actually border the rail can be tied — the tie spans the
/// (sacrificial) rail band, never a real trace groove. Bodies fenced in purely by
/// traces are counted and reported instead. Sliver components (< 1 mm²) are ignored.
fn bridge_loose_bodies(
    top_face: &MultiPolygon,
    rail_segments: &[(Point2, Point2); 4],
    bus_w: f64,
    tie_w: f64,
    pad: f64,
) -> MultiPolygon {
    use std::collections::HashMap;
    let tris: Vec<[Coord; 3]> = top_face
        .iter()
        .flat_map(triangulate_polygon)
        .collect();
    if tris.is_empty() {
        return MultiPolygon::new(vec![]);
    }

    // Group triangles into edge-connected components.
    let key = |c: &Coord| ((c.x * 1000.0).round() as i64, (c.y * 1000.0).round() as i64);
    let mut edges: HashMap<((i64, i64), (i64, i64)), Vec<usize>> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (key(&t[k]), key(&t[(k + 1) % 3]));
            edges.entry(if a <= b { (a, b) } else { (b, a) }).or_default().push(i);
        }
    }
    let n = tris.len();
    let mut adj = vec![Vec::new(); n];
    for inc in edges.values() {
        if inc.len() == 2 {
            adj[inc[0]].push(inc[1]);
            adj[inc[1]].push(inc[0]);
        }
    }
    let mut comp = vec![usize::MAX; n];
    let mut comps: Vec<Vec<usize>> = Vec::new();
    for s in 0..n {
        if comp[s] != usize::MAX {
            continue;
        }
        let id = comps.len();
        let mut stack = vec![s];
        comp[s] = id;
        let mut members = Vec::new();
        while let Some(u) = stack.pop() {
            members.push(u);
            for &w in &adj[u] {
                if comp[w] == usize::MAX {
                    comp[w] = id;
                    stack.push(w);
                }
            }
        }
        comps.push(members);
    }
    if comps.len() <= 1 {
        return MultiPolygon::new(vec![]);
    }

    let tri_area = |t: &[Coord; 3]| {
        ((t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y)).abs() / 2.0
    };
    let area_of = |c: &Vec<usize>| c.iter().map(|&i| tri_area(&tris[i])).sum::<f64>();
    let main = (0..comps.len())
        .max_by(|&i, &j| area_of(&comps[i]).total_cmp(&area_of(&comps[j])))
        .unwrap();
    let dist_to_rail = |p: Point2| {
        rail_segments
            .iter()
            .map(|(a, b)| p.distance_to(nearest_on_segment(p, *a, *b)))
            .fold(f64::INFINITY, f64::min)
    };

    let mut ties: Vec<Polygon> = Vec::new();
    let mut unbridged = 0usize;
    for (ci, members) in comps.iter().enumerate() {
        if ci == main || area_of(members) < 1.0 {
            continue;
        }
        // The component's boundary vertex that sits on the rail (within half the
        // bus width of the centerline) and is closest to it — tie there.
        let mut target: Option<(f64, Point2)> = None;
        for &ti in members {
            for v in &tris[ti] {
                let p = Point2::new(v.x, v.y);
                let d = dist_to_rail(p);
                if d <= bus_w / 2.0 + 0.25 && target.map(|(bd, _)| d < bd).unwrap_or(true) {
                    target = Some((d, p));
                }
            }
        }
        match target {
            Some((_, p)) => ties.push(rail_tie_rect(rail_segments, bus_w, tie_w, p, pad)),
            None => unbridged += 1,
        }
    }
    if unbridged > 0 {
        eprintln!(
            "⚠️  Stencil: {} small plate island(s) are enclosed by traces (not the \
             bus rail) and left un-bridged — a tie there would dam the groove. They \
             may detach when peeling; remove them by hand if so.",
            unbridged
        );
    }
    union_polys(ties)
}

/// A tie-bar rectangle that spans the bus-rail band at the centerline point nearest
/// `target`, padded past both edges so it fuses the plate on either side of the rail.
fn rail_tie_rect(
    rail_segments: &[(Point2, Point2); 4],
    bus_w: f64,
    tie_w: f64,
    target: Point2,
    pad: f64,
) -> Polygon {
    // Nearest centerline point and whether that segment runs horizontally.
    let mut best = (f64::INFINITY, target, true);
    for (a, b) in rail_segments {
        let q = nearest_on_segment(target, *a, *b);
        let d = target.distance_to(q);
        if d < best.0 {
            best = (d, q, (a.y - b.y).abs() < (a.x - b.x).abs());
        }
    }
    let (_, c, horizontal) = best;
    let half = bus_w / 2.0 + pad;
    if horizontal {
        rect_poly(c.x - tie_w / 2.0, c.y - half, c.x + tie_w / 2.0, c.y + half)
    } else {
        rect_poly(c.x - half, c.y - tie_w / 2.0, c.x + half, c.y + tie_w / 2.0)
    }
}

/// Re-orient an edge-manifold mesh so every triangle winds consistently and all
/// normals point outward. Flood-fills winding agreement across shared edges, then
/// flips globally if the enclosed signed volume came out negative. This frees the
/// face/wall generators from having to agree on winding up front — they only need
/// to produce an edge-paired (watertight) surface.
fn make_outward_consistent(mesh: &mut Mesh3D) {
    use std::collections::HashMap;
    // Drop duplicate-vertex degenerate triangles (zero-area slivers earcut can
    // emit around hole rings). They contribute no surface and each self-pairs its
    // edges, so removing them keeps the rest watertight — and it keeps the
    // edge-adjacency below clean (no self-edges).
    mesh.triangles
        .retain(|t| t.vertices[0] != t.vertices[1] && t.vertices[1] != t.vertices[2] && t.vertices[2] != t.vertices[0]);
    let n = mesh.triangles.len();
    if n == 0 {
        return;
    }
    let key = |v: [f32; 3]| (v[0].to_bits(), v[1].to_bits(), v[2].to_bits());

    // Undirected edge → the (triangle, directed a→b) incidences that share it.
    type V = (u32, u32, u32);
    let mut edges: HashMap<(V, V), Vec<(usize, V, V)>> = HashMap::new();
    for (ti, t) in mesh.triangles.iter().enumerate() {
        for k in 0..3 {
            let a = key(t.vertices[k]);
            let b = key(t.vertices[(k + 1) % 3]);
            let und = if a <= b { (a, b) } else { (b, a) };
            edges.entry(und).or_default().push((ti, a, b));
        }
    }

    // Adjacency with an "already consistent?" flag (shared edge runs opposite ways).
    let mut adj: Vec<Vec<(usize, bool)>> = vec![Vec::new(); n];
    for inc in edges.values() {
        if inc.len() == 2 {
            let (t0, a0, b0) = inc[0];
            let (t1, a1, b1) = inc[1];
            let consistent = a0 == b1 && b0 == a1;
            adj[t0].push((t1, consistent));
            adj[t1].push((t0, consistent));
        }
    }

    // Flood-fill a flip flag across every connected component.
    let mut flip = vec![false; n];
    let mut seen = vec![false; n];
    for start in 0..n {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        while let Some(t) = stack.pop() {
            for &(nb, consistent) in &adj[t] {
                if !seen[nb] {
                    seen[nb] = true;
                    flip[nb] = if consistent { flip[t] } else { !flip[t] };
                    stack.push(nb);
                }
            }
        }
    }
    for (ti, t) in mesh.triangles.iter_mut().enumerate() {
        if flip[ti] {
            t.vertices.swap(1, 2);
        }
    }

    // Orient outward: a closed surface with outward normals encloses positive volume.
    let vol: f64 = mesh
        .triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.vertices;
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0])) as f64
        })
        .sum();
    if vol < 0.0 {
        for t in mesh.triangles.iter_mut() {
            t.vertices.swap(1, 2);
        }
    }

    // Recompute normals from the final winding.
    for t in mesh.triangles.iter_mut() {
        let e1 = sub(t.vertices[1], t.vertices[0]);
        let e2 = sub(t.vertices[2], t.vertices[0]);
        let nrm = cross(e1, e2);
        let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
        t.normal = if len < 1e-10 {
            [0.0, 0.0, 1.0]
        } else {
            [nrm[0] / len, nrm[1] / len, nrm[2] / len]
        };
    }
}

