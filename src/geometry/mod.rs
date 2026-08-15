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

use crate::config::{ChannelProfile, Config, StencilMount, ViaStyle};
use crate::pcb::{BoardOutline, CopperLayer, CutoutShape, Pad, PadShape, PcbData, Point2, Trace, Via};

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

    // `eyelet_style = "indent"` and `--no-via-indents` never produced any
    // geometry — vias have always been cut as plain through-holes — but the
    // options, and a warning about indent depth, implied otherwise. Say so
    // rather than continuing to imply a dimple that is not there.
    if config.eyelet_style == crate::config::EyeletStyle::Indent
        || !config.generate_via_indents
    {
        eprintln!(
            "⚠️  eyelet_style / --no-via-indents have no effect and are deprecated: vias have \
             always been cut as full through-holes."
        );
        eprintln!("   Use --via-style straight|cone to choose the barrel shape instead.");
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

    // Channel cross-section bands, narrowest (floor) first. A rectangular
    // profile is a single band, which makes everything below collapse onto the
    // original straight-walled behaviour.
    let floor_w = channel_floor_width(config);
    // A rectangular profile needs only one band — but a cone is *built* from
    // the band stack, so it needs several regardless of the groove profile.
    let bands = if config.channel_profile == ChannelProfile::Rect
        && config.via_style != ViaStyle::Cone
    {
        1
    } else {
        taper_band_count(config.channel_depth_mm, config.taper_slice_height_mm)
    };
    if bands > 1 {
        // The wall angle is a consequence of opening width, floor width and
        // depth rather than something set directly, so report what came out.
        // It governs overhang: a bottom-face groove closes as it rises, and a
        // wall shallower than ~45° from horizontal will need support.
        let angle = taper_wall_angle_deg(chan_w, floor_w, config.channel_depth_mm);
        eprintln!(
            "ℹ️  {} channel profile: {:.2}mm opening → {:.2}mm floor over {:.2}mm \
             ({:.0}° walls, {} bands)",
            config.channel_profile, chan_w, floor_w, config.channel_depth_mm, angle, bands
        );
        if angle < 45.0 {
            eprintln!(
                "   Walls are shallower than 45° — bottom-face grooves may need support."
            );
        }
    }

    // A rectangular groove has no taper of its own, so if cones have forced a
    // multi-band stack its walls would repeat identically band after band —
    // shared boundaries, zero-width ledge spurs, non-manifold edges. Give it
    // the same minimum growth every other feature gets. Over the whole stack
    // that is a few hundredths of a millimetre of draft, far below what the
    // nozzle resolves, and it keeps the opening width exact.
    let floor_w = if config.channel_profile == ChannelProfile::Rect && bands > 1 {
        (chan_w - 2.0 * MIN_BAND_GROWTH_MM * (bands - 1) as f64).max(MIN_TAPER_FLOOR_MM.min(chan_w))
    } else {
        floor_w
    };

    // Interpolate so band 0 lands exactly on the floor width and the last band
    // exactly on the opening width — the latter matters because the face cut
    // and the stencil both key off the opening.
    let band_width = |i: usize| {
        if bands <= 1 {
            chan_w
        } else {
            floor_w + (chan_w - floor_w) * (i as f64 / (bands - 1) as f64)
        }
    };

    // Pad lands: shallow, pad-shaped indents (same depth as trace channels) so
    // electroplating fills a properly shaped, solderable pad — not just the
    // lead's round drill hole. Merged into each layer's channel network so a
    // trace flows continuously into its pad. THT pads still get a real
    // through-hole for the lead (below) cut through the middle of this indent.
    //
    // Cone barrels are built as part of the channel band stack rather than as a
    // separate full-thickness sweep of the whole board.
    //
    // The reason is measured, not aesthetic. A whole-board sweep computes each
    // ledge across the *entire* void — every trace, every bore, every cutout —
    // at every height, and the defect count scaled directly with how many such
    // ledges there were. This stack computes ledges only within the channel
    // network, which is the arrangement that already regenerates watertight for
    // straight bores on every profile.
    //
    // A cone mouth is, geometrically, exactly what `hole_collars` already
    // produces: a per-bore footprint that grows monotonically band by band.
    // So a countersink is just a collar whose radius follows the cone profile,
    // and it inherits the strict nesting that keeps the mesh closed.
    let cone_bores = if config.via_style == ViaStyle::Cone {
        plan_bores(pcb, config, outline)
    } else {
        Vec::new()
    };

    // Every feature in a band — trace width, pad land, hole collar — grows
    // monotonically from the floor band to the opening band, so consecutive
    // bands nest *strictly*: no shared boundary anywhere.
    //
    // That strictness is what keeps the mesh closed. `add_tapered_channel`
    // joins consecutive bands with a ledge computed as a polygon difference,
    // and wherever two band boundaries touch, `geo` inserts intersection
    // vertices into that ledge which exist in neither band's own ring. The
    // resulting T-junctions leave edges without a partner — an open mesh, which
    // a slicer is free to render as a corked hole or a flat plaque. Keeping the
    // bands strictly nested means the ledge is a plain annulus bounded by the
    // two rings exactly as the walls draw them.
    //
    // Lands shrink toward the floor by the same amount the channel does, and
    // are dropped entirely once they fall below a printable size.
    let band_shrink = |i: usize| band_width(i) - chan_w; // ≤ 0, exactly 0 at the top
    // 0 at the floor band, 1 at the opening band.
    let band_frac = |i: usize| if bands <= 1 { 1.0 } else { i as f64 / (bands - 1) as f64 };
    let build_levels = |traces: &[Trace], on_layer: &dyn Fn(&Pad) -> bool, what: &str| {
        (0..bands)
            .map(|i| {
                let mut level = union_traces(traces, band_width(i));
                if config.generate_pad_lands {
                    let lands = union_pad_lands(&pcb.pads, band_shrink(i), on_layer);
                    level = safe_union(level, &lands, what);
                }
                if cone_bores.is_empty() {
                    // Collars apply to the rectangular profile too. It has only
                    // one band, so it cannot suffer band-to-band T-junctions, but
                    // its single boundary still lands on the bore and coincides
                    // with the hole ring the face cut and the barrel wall both use.
                    let collars = hole_collars(
                        pcb,
                        config,
                        traces,
                        chan_w,
                        on_layer,
                        config.generate_pad_lands,
                        // Grows with the band, keeping collars strictly nested too.
                        (band_width(i) - band_width(0)) / 2.0,
                    );
                    level = safe_union(level, &collars, what);
                } else {
                    // The countersink itself, widest at the opening band. This
                    // supersedes the plain collar: it is the same shape, just
                    // sized by the cone profile instead of by the channel.
                    let depth = chan_depth as f64 * (1.0 - band_frac(i));
                    let cones = union_cone_footprints(&cone_bores, depth, i);
                    level = safe_union(level, &cones, what);
                }
                level
            })
            .collect::<Vec<_>>()
    };
    let fcu_levels = build_levels(&pcb.traces_fcu, &|p: &Pad| p.on_fcu, "F.Cu network");
    let bcu_levels = build_levels(&pcb.traces_bcu, &|p: &Pad| p.on_bcu, "B.Cu network");

    // Pad holes: each pad uses its own drill size from KiCad.
    // hole_r serves as a minimum (in case a pad has a tiny or missing drill value).
    let holes = if config.generate_pad_holes {
        union_pad_holes(&pcb.pads, hole_r, 16)
    } else {
        MultiPolygon::new(vec![])
    };

    // Via holes: always treat as through-holes (merged into pad holes polygon)
    let via_holes = if !pcb.vias.is_empty() {
        union_via_holes(&pcb.vias, config, 16)
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
    //
    // With a tapered profile there are several bands to clip, not one. Only the
    // widest gets the full two-op treatment; the narrower bands are subsets of
    // the *unclipped* widest, so intersecting each against the *clipped* widest
    // applies both clips at once — one boolean op per band instead of two, and
    // it also guarantees the containment `add_tapered_channel` relies on to
    // build its ledges.
    //
    // Every band is clipped the same way rather than the narrow ones being
    // intersected against the widest: they already share their collars, and
    // intersecting would make their boundaries coincide with the widest band's,
    // reintroducing exactly the T-junctions the collars exist to prevent.
    let clip_levels = |levels: Vec<MultiPolygon>, what: &str| -> Vec<MultiPolygon> {
        levels
            .into_iter()
            .map(|l| {
                let l = safe_intersection(l, &board_mp, what);
                safe_difference(l, &all_holes, what)
            })
            .collect()
    };
    let fcu_levels = clip_levels(fcu_levels, "F.Cu network clip");
    let bcu_levels = clip_levels(bcu_levels, "B.Cu network clip");
    // The opening band is what the face cut and the stencil key off.
    let fcu = fcu_levels.last().expect("at least one F.Cu band").clone();
    let bcu = bcu_levels.last().expect("at least one B.Cu band").clone();

    // ── Top face (z = thickness, normal +Z) ────────────────────────────────
    let top_face = safe_difference(solid_substrate.clone(), &fcu, "top face - F.Cu network");
    add_flat(&mut mesh, &top_face, &ctx, thickness, true);

    // ── Bottom face (z = 0, normal −Z) ─────────────────────────────────────
    let has_bottom_recess = !pcb.traces_bcu.is_empty() || config.via_style == ViaStyle::Cone;
    let bot_face = if !has_bottom_recess {
        solid_substrate.clone()
    } else {
        safe_difference(solid_substrate.clone(), &bcu, "bottom face - B.Cu network")
    };
    add_flat(&mut mesh, &bot_face, &ctx, 0.0, false);

    // ── Side walls (z = 0 → thickness) ─────────────────────────────────────
    add_outline_walls(&mut mesh, outline, &ctx, 0.0, thickness);

    // ── F.Cu channel floors + inner walls ──────────────────────────────────
    add_tapered_channel(&mut mesh, &fcu_levels, &ctx, thickness - chan_depth, thickness, true);

    // ── B.Cu channel floors + inner walls ──────────────────────────────────
    if has_bottom_recess {
        add_tapered_channel(&mut mesh, &bcu_levels, &ctx, chan_depth, 0.0, false);
    }

    // ── Through-hole cylinder walls (pads + vias) ──────────────────────────
    // Walk the all_holes polygon rings directly — exact same vertices as the face holes.
    for poly in all_holes.iter() {
        add_ring_walls(&mut mesh, poly.exterior().coords(), 0.0, thickness, false, &ctx);
        for interior in poly.interiors() {
            add_ring_walls(&mut mesh, interior.coords(), 0.0, thickness, true, &ctx);
        }
    }

    // Force a single, consistently-outward orientation so the STL slices
    // cleanly. Wall winding here is best-effort against the earcut-triangulated
    // faces (same issue fixed for generate_stencil() in b798c21): without this,
    // Cura's slicer can fill pad/via holes solid even though the preview shows
    // them open, because some hole-wall triangles face the wrong way.
    make_outward_consistent(&mut mesh);

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

/// Bore diameter to cut for a via, in millimeters.
///
/// A via's KiCad drill is the *electrical* hole — typically 0.3–0.4 mm, far
/// smaller than an eyelet barrel and below what an FDM nozzle can hold open.
/// `eyelet_diameter_mm` is therefore a floor. But it must not also be a
/// ceiling: the previous code assigned every via that one fixed diameter and
/// discarded `via.drill` entirely, so a deliberately large via got silently
/// shrunk. Take whichever is larger.
fn via_bore_diameter(via: &Via, config: &Config) -> f64 {
    via.drill.max(config.eyelet_diameter_mm)
}

/// Union of via bores, each at its own diameter (see `via_bore_diameter`).
fn union_via_holes(vias: &[Via], config: &Config, sides: usize) -> MultiPolygon {
    union_polys(
        vias.iter()
            .map(|v| circle_poly(&v.center, via_bore_diameter(v, config) / 2.0, sides))
            .collect(),
    )
}

/// Union of pad hole circles, using each pad's own drill diameter (from KiCad).
/// `min_radius` is a floor in case a pad has a missing or unrealistically small drill.
/// `min_radius` is a floor applied only to pads that already have a real KiCad
/// drill (guards against a tiny/degenerate drill value) — a pad with no drill
/// at all (`drill == 0.0`, e.g. an SMD pad) is skipped entirely and never gets
/// a fabricated hole; it may still get a shaped land indent (`pad_land_poly`).
/// Slotted `(drill oval W L)` pads produce a stadium rather than a circle,
/// oriented by the pad's absolute rotation — a round hole at the slot's width
/// would be too short for the lead, and one at its length too wide.
fn union_pad_holes(pads: &[Pad], min_radius: f64, sides: usize) -> MultiPolygon {
    union_polys(
        pads.iter()
            .filter(|p| p.drill > 0.0)
            .map(|p| {
                let min_d = min_radius * 2.0;
                let w = p.drill.max(min_d);
                // drill_h == drill for an ordinary round drill. Guard against
                // an unset drill_h on any Pad built before this field existed.
                let h = if p.drill_h > 0.0 { p.drill_h.max(min_d) } else { w };
                if (w - h).abs() < 1e-9 {
                    circle_poly(&p.center, w / 2.0, sides)
                } else {
                    oval_poly(p.center.x, p.center.y, w, h, p.rotation_deg, sides / 2)
                }
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
/// A negative `margin_mm` shrinks the land instead, which is how a tapered
/// channel steps its lands down toward the floor. A land shrunk below one
/// nozzle track is dropped rather than emitted as a sliver.
fn pad_land_poly(pad: &Pad, margin_mm: f64) -> Option<Polygon> {
    if pad.width <= 0.0 || pad.height <= 0.0 {
        return None;
    }
    let w = pad.width + margin_mm;
    let h = pad.height + margin_mm;
    if w < MIN_TAPER_FLOOR_MM || h < MIN_TAPER_FLOOR_MM {
        return None;
    }
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

/// Narrowest *flat floor* a trapezoid is allowed, in millimeters.
///
/// Roughly one 0.4 mm nozzle track. A trapezoid's floor is deliberate — it
/// exists to keep copper cross-section — so it has to be a width the printer
/// can actually lay down. Also the width below which a pad land is dropped
/// rather than emitted as a sliver.
///
/// This does *not* apply to a vee: see `VEE_APEX_WIDTH_MM`.
const MIN_TAPER_FLOOR_MM: f64 = 0.4;

/// Width the vee profile converges to, in millimeters.
///
/// Deliberately well below one nozzle track, and that is the whole point of the
/// profile. A vee is meant to *converge*; where the groove narrows past what
/// the extruder can lay, the slicer simply stops opening it and prints solid,
/// so the printed groove ends in a naturally truncated point. Clamping the
/// model to a printable floor instead would carve a flat shelf at the bottom —
/// which is precisely what a trapezoid is, and why the two profiles used to
/// produce byte-identical output at stock settings.
///
/// Not zero, because a zero-width band is degenerate geometry: the bands have
/// to stay strictly nested and non-empty for the mesh to close. 0.1 mm is far
/// below anything a 0.4 mm nozzle renders, yet a thousand times the 1e-4 mm
/// grid the mesh welds on, so it is unambiguous geometry.
const VEE_APEX_WIDTH_MM: f64 = 0.1;

/// Upper bound on bands in a single taper.
///
/// Each band costs a polygon union plus an intersection, and the underlying
/// `geo` boolean-op sweep is both the slowest and the least robust part of the
/// pipeline (it can hang on touching polygons, and silently drop boundary
/// detail on dense trace sets). Beyond ~8 steps the extra fidelity is well
/// below a printed layer anyway, so it buys nothing but risk.
const MAX_TAPER_BANDS: usize = 8;

/// Full-depth collars around the through-holes that a layer's traces run into.
///
/// A taper must not run a knife edge into a bore. Each band would otherwise be
/// clipped by the hole circle at a different place — a narrow band is severed
/// where a wide one is merely pinched — and those differing clip points insert
/// different vertices into the same circle. The bands then meet the bore at
/// T-junctions whose edges have no partner, leaving the mesh open exactly where
/// a slicer is most likely to respond by corking the hole.
///
/// A collar makes the bore strictly interior to *every* band, so no band's
/// boundary ever lands on it. Two properties are load-bearing and easy to break:
///
/// - The collar must go into **every** band, the widest included. Putting it
///   only in the narrow ones makes their boundaries coincide with the widest
///   band's instead, which just moves the T-junctions outward.
/// - The collar must **not** be clipped against the widest band, for the same
///   reason — clipping is what makes the boundaries coincide.
///
/// Only holes that a trace actually reaches get one, so an isolated via keeps
/// its plain bore instead of gaining a pocket. Radius is at least half the
/// channel width, so a collar never pinches the groove it sits in.
///
/// This is also the right shape physically: a flat-bottomed pocket around the
/// hole, like a pad land, rather than a taper pinching out into the barrel.
/// `extra_r` grows the collar for higher bands. Bands must nest *strictly*, so
/// every feature in them — collars included — has to grow monotonically; a
/// collar that were the same size in two bands would put their boundaries on
/// top of each other, which is the very thing this is here to avoid.
fn hole_collars(
    pcb: &PcbData,
    config: &Config,
    traces: &[Trace],
    chan_w: f64,
    on_layer: impl Fn(&Pad) -> bool,
    lands_enabled: bool,
    extra_r: f64,
) -> MultiPolygon {
    // A hole is "reached" when its bore comes within half a channel width of a
    // trace centreline on this layer — the same threshold at which the widest
    // band starts to overlap the bore, so collars appear exactly when needed.
    let reached = |center: Point2, bore_r: f64| {
        traces.iter().any(|t| {
            let n = nearest_on_segment(center, t.start, t.end);
            let dx = n.x - center.x;
            let dy = n.y - center.y;
            (dx * dx + dy * dy).sqrt() < bore_r + chan_w / 2.0
        })
    };

    let mut polys = Vec::new();
    for p in pcb.pads.iter().filter(|p| p.drill > 0.0) {
        let bore_r = p.drill.max(p.drill_h).max(config.pad_hole_diameter_mm) / 2.0;
        // A pad land sits over its own bore, so a landed pad needs a collar
        // even with no trace running into it.
        if reached(p.center, bore_r) || (lands_enabled && on_layer(p)) {
            polys.push(circle_poly(&p.center, bore_r + COLLAR_GAP_MM + extra_r, 16));
        }
    }
    for v in &pcb.vias {
        let bore_r = via_bore_diameter(v, config) / 2.0;
        if reached(v.center, bore_r) {
            polys.push(circle_poly(&v.center, bore_r + COLLAR_GAP_MM + extra_r, 16));
        }
    }
    union_polys(polys)
}

/// Resolves the configured profile to an actual groove floor width in mm.
///
/// Always clamped into `[MIN_TAPER_FLOOR_MM, channel_width_mm]`: a floor wider
/// than the opening would invert the taper, and one narrower than a nozzle
/// track cannot print.
fn channel_floor_width(config: &Config) -> f64 {
    let top = config.channel_width_mm;
    match config.channel_profile {
        ChannelProfile::Rect => top,
        // A trapezoid's floor is intentional, so it must be printable.
        ChannelProfile::Trapezoid => config
            .channel_floor_width_mm
            .clamp(MIN_TAPER_FLOOR_MM.min(top), top),
        // A vee converges as far as the mesh can represent and lets the printer
        // decide where to truncate it. `channel_floor_width_mm` does not apply.
        ChannelProfile::Vee => VEE_APEX_WIDTH_MM.min(top),
    }
}

/// Number of constant-width bands to approximate a taper of `depth_mm`.
///
/// Returns 1 for a degenerate or rectangular taper, which makes the tapered
/// path collapse exactly onto the original single-band behaviour.
fn taper_band_count(depth_mm: f64, slice_height_mm: f64) -> usize {
    if depth_mm <= 0.0 || slice_height_mm <= 0.0 {
        return 1;
    }
    ((depth_mm / slice_height_mm).ceil() as usize).clamp(1, MAX_TAPER_BANDS)
}

/// The wall angle a taper actually achieves, in degrees from the horizontal
/// floor — 90° being a vertical wall. Reported to the user rather than
/// configured, since the opening width, floor width and depth already
/// determine it.
fn taper_wall_angle_deg(top_w: f64, floor_w: f64, depth: f64) -> f64 {
    let run = (top_w - floor_w) / 2.0;
    if run <= 1e-9 {
        return 90.0;
    }
    depth.atan2(run).to_degrees()
}

/// Builds a tapered channel from a stack of constant-width bands.
///
/// `levels` runs floor-first, opening-last, and each entry must contain the
/// one before it (`levels[i] ⊆ levels[i+1]`) — the caller guarantees this by
/// intersecting every level against the widest one. Band `i` holds the
/// cross-section `levels[i]` over its whole height, and consecutive bands are
/// joined by a horizontal ledge, so the wall is a staircase rather than a true
/// ramp. That is not an approximation in the printed part: an FDM slicer
/// quantises the wall to its layer height anyway.
///
/// Winding follows `add_channel`: ledges face the same way as the floor, and
/// walls face into the groove.
fn add_tapered_channel(
    mesh: &mut Mesh3D,
    levels: &[MultiPolygon],
    ctx: &Ctx,
    z_floor: f32,
    z_opening: f32,
    is_top: bool,
) {
    let n = levels.len();
    if n == 0 {
        return;
    }
    // A single band is exactly the original straight-walled groove.
    if n == 1 {
        add_channel(mesh, &levels[0], ctx, z_floor, z_opening, is_top);
        return;
    }

    // Floor sits at the narrowest level.
    add_flat(mesh, &levels[0], ctx, z_floor, is_top);

    let band_z = |i: usize| z_floor + (z_opening - z_floor) * (i as f32 / n as f32);

    for (i, level) in levels.iter().enumerate() {
        let (z0, z1) = (band_z(i), band_z(i + 1));

        for poly in level.iter() {
            add_ring_walls(mesh, poly.exterior().coords(), z0, z1, !is_top, ctx);
            for interior in poly.interiors() {
                add_ring_walls(mesh, interior.coords(), z0, z1, is_top, ctx);
            }
        }

        // Ledge joining this band's rim to the next, wider one. Its exterior is
        // the next level's ring and its hole is this level's, so both edges
        // share vertices with the walls above and below after snapping.
        if let Some(next) = levels.get(i + 1) {
            let ledge =
                drop_slivers(safe_difference(next.clone(), level, "channel taper ledge"));
            add_flat(mesh, &ledge, ctx, z1, is_top);
        }
    }
}

// ---------------------------------------------------------------------------
// Double-cone through-holes
// ---------------------------------------------------------------------------

/// Facet count for a circle or stadium of the given radius.
///
/// A fixed 16-gon is fine on a 0.8 mm bore (0.16 mm facets) but leaves 0.6 mm
/// facets on a 3 mm cone mouth — coarse enough to see, and to have to clean up.
/// Targets a roughly constant facet length instead.
fn arc_sides_for(radius: f64) -> usize {
    const TARGET_FACET_MM: f64 = 0.15;
    let n = (std::f64::consts::TAU * radius / TARGET_FACET_MM).ceil() as usize;
    n.clamp(16, 64)
}

/// Clearance between a bore and the collar (or cone footprint) that surrounds
/// it, at the narrowest band. It only has to be enough to keep the bore off the
/// band boundary — but the exact value matters, and not for the reason it looks
/// like.
///
/// Coincident feature boundaries are what break these meshes, and a collar's
/// radius is `bore_r + this`. Drill diameters and channel widths are both round
/// numbers, so a round gap makes the collar land exactly on the channel's
/// half-width for some perfectly ordinary drill: at 0.15 a 0.9 mm drill gives a
/// 0.6 mm collar, precisely the half-width of a 1.2 mm channel, and the two
/// circles coincide. An off-grid value means that collision would need a
/// 0.34 mm difference, which round drill and channel sizes do not produce.
const COLLAR_GAP_MM: f64 = 0.17;

/// Smallest amount by which a feature must grow from one band to the next, in
/// millimeters. Guarantees the strict nesting the ledge construction depends
/// on, even for a cone that clearance limiting has flattened away. Tiny
/// compared to `min_rim_mm`, so it cannot close a clearance gap.
///
/// Off-grid for the same reason as `COLLAR_GAP_MM`: at a round 0.010 one
/// feature landed exactly tangent to another and produced a pinch point — a
/// single vertical edge carrying four faces. Values that are not round
/// fractions of a millimetre do not coincide with drill and channel sizes.
const MIN_BAND_GROWTH_MM: f64 = 0.011;

/// One through-hole, with its cone mouth already sized to fit its surroundings.
#[derive(Debug, Clone)]
struct Bore {
    center: Point2,
    /// Bore size at the throat, in the pad's local frame. Equal for a round
    /// drill; a slot keeps its two dimensions.
    w: f64,
    h: f64,
    rotation_deg: f64,
    /// How far the mouth reaches beyond the bore wall, **per outline vertex**.
    ///
    /// One scalar per vertex rather than a single radius, so a mouth can bulge
    /// where there is room and pull in only where something is close. A hole
    /// with one tight neighbour used to lose that clearance on all four sides;
    /// now it goes oval, growing away from the obstruction. On a pin row —
    /// where the measurement said 64 of 77 constrained mouths were limited by
    /// another hole — that recovers most of the wasted area.
    mouth_offsets: Vec<f64>,
    /// Depth each cone descends from its face.
    cone_depth: f64,
    /// Facet count, fixed per bore so every band of one barrel has matching
    /// vertices. Bands that disagreed would not pair into a closed surface.
    sides: usize,
}

impl Bore {
    /// Footprint at a given depth below the face the cone opens onto.
    ///
    /// Built radially about the bore centre: vertex *i* sits at
    /// `centre + direction_i * (base_radius_i + offset_i)`. Because every
    /// radius is positive the result is star-shaped about the centre, and so
    /// always a simple polygon however unevenly the offsets vary.
    ///
    /// All offsets scale by the same band factor, which is what preserves the
    /// strict nesting between bands that keeps the mesh closed — the shape
    /// changes, the nesting property does not.
    fn poly_at_depth(&self, depth_below_face: f64, band: usize) -> Polygon {
        let t = self.band_fraction(depth_below_face);
        let base = self.base_outline();
        let coords: Vec<Coord> = base
            .iter()
            .enumerate()
            .map(|(i, &(dir_x, dir_y, base_r))| {
                let off = self.offset_at(i, t, band);
                let r = base_r + off;
                Coord { x: self.center.x + dir_x * r, y: self.center.y + dir_y * r }
            })
            .chain(std::iter::once_with(|| {
                let (dx, dy, br) = base[0];
                let r = br + self.offset_at(0, t, band);
                Coord { x: self.center.x + dx * r, y: self.center.y + dy * r }
            }))
            .collect();
        Polygon::new(LineString::new(coords), vec![])
    }

    /// Unit direction and bore radius for each outline vertex, evenly spaced in
    /// angle. A round drill has a constant radius; a slot's varies, which keeps
    /// its mouth a dilated stadium rather than a circle.
    fn base_outline(&self) -> Vec<(f64, f64, f64)> {
        let rot = self.rotation_deg.to_radians();
        let (a, b) = (self.w / 2.0, self.h / 2.0);
        (0..self.sides)
            .map(|i| {
                let ang = std::f64::consts::TAU * i as f64 / self.sides as f64;
                let (dx, dy) = (ang.cos(), ang.sin());
                // Radius of the bore in this direction, in the pad's own frame.
                let (lx, ly) = (dx * rot.cos() + dy * rot.sin(), -dx * rot.sin() + dy * rot.cos());
                let base_r = if (a - b).abs() < 1e-9 {
                    a
                } else {
                    // Stadium: the straight flanks are `min(a,b)` from the axis,
                    // the caps a further `|a-b|` along it.
                    let half_straight = (a - b).abs();
                    let r = a.min(b);
                    if a >= b {
                        (lx.abs() * half_straight + r).max(r)
                    } else {
                        (ly.abs() * half_straight + r).max(r)
                    }
                };
                let (gx, gy) = (dx, dy);
                (gx, gy, base_r)
            })
            .collect()
    }

    fn band_fraction(&self, depth_below_face: f64) -> f64 {
        if self.cone_depth <= 0.0 {
            0.0
        } else {
            (1.0 - depth_below_face / self.cone_depth).clamp(0.0, 1.0)
        }
    }

    /// Offset for one vertex, at cone fraction `t` and band index `band`.
    fn offset_at(&self, vertex: usize, t: f64, band: usize) -> f64 {
        let mouth = self.mouth_offsets.get(vertex).copied().unwrap_or(0.0);
        let cone = (mouth - COLLAR_GAP_MM).max(0.0) * t;
        // Every feature in the band stack must grow *strictly* from one band to
        // the next. A footprint that repeats gives two levels a shared boundary
        // segment, and the ledge between them then comes out as a zero-width
        // spur whose two sides each bound a face — four faces on one edge, which
        // is non-manifold and invisible to the sliver filter, since the ledge
        // polygon is otherwise perfectly legitimate.
        //
        // Clearance limiting makes this the common case rather than a corner
        // one: a mouth shrunk to at or below the collar gap has no cone left to
        // grow, so its footprint would otherwise be identical in every band.
        // This floor is small enough that a mouth held off its neighbour by
        // `min_rim_mm` still clears it several times over.
        COLLAR_GAP_MM + cone.max(MIN_BAND_GROWTH_MM * band as f64)
    }

    /// Largest mouth offset over all directions — for reporting only.
    fn max_mouth_offset(&self) -> f64 {
        self.mouth_offsets.iter().copied().fold(0.0, f64::max)
    }
}

/// Union of every cone footprint at a given depth below its face.
fn union_cone_footprints(bores: &[Bore], depth_below_face: f64, band: usize) -> MultiPolygon {
    union_polys(bores.iter().map(|b| b.poly_at_depth(depth_below_face, band)).collect())
}

/// How far a mouth may reach from `from` along `dir` before an obstacle stops it.
///
/// The obstacle is a disc of radius `keep_out` centred at `obs`; the answer is
/// where the ray first meets it. `None` means this direction never meets the
/// obstacle and so is unconstrained by it — which is the entire point. A
/// neighbour to the east must not shrink the mouth to the west, and treating a
/// behind-the-ray intersection as a hit is exactly the isotropic behaviour this
/// replaces.
fn ray_limit(from: Point2, dir: (f64, f64), obs: Point2, keep_out: f64) -> Option<f64> {
    let (bx, by) = (obs.x - from.x, obs.y - from.y);
    let along = bx * dir.0 + by * dir.1;
    let d2 = bx * bx + by * by;
    if d2 < keep_out * keep_out {
        return Some(0.0); // already inside the keep-out; no room at all
    }
    let disc = along * along - (d2 - keep_out * keep_out);
    if disc <= 0.0 {
        return None; // ray passes clear of the obstacle
    }
    let hit = along - disc.sqrt();
    if hit < 0.0 {
        return None; // both intersections lie behind the ray origin
    }
    Some(hit)
}

/// Plans a cone for every through-hole, shrinking each mouth until it clears
/// its neighbours.
///
/// Cone mouths are much wider than the bores they surround: a 0.8 mm hole with
/// 45° walls through a 2.2 mm board wants a ~3 mm crater on each face. On
/// 2.54 mm pin pitch two of those overlap, which would short the pins together
/// through the plating — so this is a correctness constraint, not a nicety.
///
/// Each mouth is limited by whichever is nearest: the board edge, another hole
/// on a *different* net, or a foreign-net trace. Holes sharing a net may merge
/// freely — they are the same conductor, so a merged mouth is harmless and
/// often desirable. A mouth squeezed to nothing simply leaves a straight bore.
fn plan_bores(pcb: &PcbData, config: &Config, outline: &BoardOutline) -> Vec<Bore> {
    let thickness = config.substrate_thickness_mm;
    let min_rim = config.min_rim_mm;
    let chan_w = config.channel_width_mm;

    // Cones descend from each face and meet at a short straight throat.
    // Bounded by the channel depth: the countersink is emitted as part of the
    // channel band stack, so it cannot reach deeper than that stack goes. The
    // throat setting still applies, whichever is the tighter limit.
    let desired_depth = ((thickness - config.throat_height_mm) / 2.0)
        .min(config.channel_depth_mm)
        .max(0.0);
    // Horizontal run for that depth at the configured wall angle.
    let angle = config.cone_angle_deg.clamp(15.0, 89.0).to_radians();
    let desired_offset = desired_depth / angle.tan();

    // Collect every bore first; clearance limiting needs to see them all.
    struct Raw {
        center: Point2,
        w: f64,
        h: f64,
        rot: f64,
        net: Option<String>,
    }
    let mut raws: Vec<Raw> = Vec::new();
    if config.generate_pad_holes {
        let min_d = config.pad_hole_diameter_mm;
        for p in pcb.pads.iter().filter(|p| p.drill > 0.0) {
            let w = p.drill.max(min_d);
            let h = if p.drill_h > 0.0 { p.drill_h.max(min_d) } else { w };
            raws.push(Raw {
                center: p.center,
                w,
                h,
                rot: p.rotation_deg,
                net: p.net_name.clone(),
            });
        }
    }
    for v in &pcb.vias {
        let d = via_bore_diameter(v, config);
        raws.push(Raw { center: v.center, w: d, h: d, rot: 0.0, net: v.net_name.clone() });
    }

    // Distance from a point to the board edge.
    let edge_distance = |p: Point2| -> f64 {
        let verts = &outline.vertices;
        let n = verts.len();
        (0..n)
            .map(|i| {
                let near = nearest_on_segment(p, verts[i], verts[(i + 1) % n]);
                ((near.x - p.x).powi(2) + (near.y - p.y).powi(2)).sqrt()
            })
            .fold(f64::INFINITY, f64::min)
    };

    let all_traces = pcb.traces_fcu.iter().chain(pcb.traces_bcu.iter());

    // Per-bore vertex directions and the isotropic starting allowance.
    let sides_of: Vec<usize> = raws
        .iter()
        .map(|r| arc_sides_for(r.w.max(r.h) / 2.0 + desired_offset))
        .collect();
    let dirs_of: Vec<Vec<(f64, f64)>> = sides_of
        .iter()
        .map(|&n| {
            (0..n)
                .map(|k| {
                    let a = std::f64::consts::TAU * k as f64 / n as f64;
                    (a.cos(), a.sin())
                })
                .collect()
        })
        .collect();

    // Start every direction at the full desired reach, limited only by the
    // board edge — that one is treated isotropically because it never bound on
    // any real board measured, and a per-direction outline distance costs far
    // more than the constraint is worth.
    let mut offsets: Vec<Vec<f64>> = raws
        .iter()
        .zip(&sides_of)
        .map(|(r, &n)| {
            let bore_r = r.w.max(r.h) / 2.0;
            let edge_lim = (edge_distance(r.center) - bore_r - min_rim).max(0.0);
            vec![desired_offset.min(edge_lim); n]
        })
        .collect();

    // Foreign-net traces. These never grow, so one pass settles them.
    //
    // A trace is a segment, not a point, so it is sampled along its length
    // rather than reduced to its nearest point — a mouth growing *along* a
    // trace would otherwise run into a part of it the nearest point never saw.
    let keep_out_trace = chan_w / 2.0 + min_rim;
    for (i, raw) in raws.iter().enumerate() {
        let bore_r = raw.w.max(raw.h) / 2.0;
        let reach = bore_r + desired_offset + keep_out_trace;
        for t in all_traces.clone() {
            let near = nearest_on_segment(raw.center, t.start, t.end);
            let d = ((near.x - raw.center.x).powi(2) + (near.y - raw.center.y).powi(2)).sqrt();
            // Traces carry no net in the parsed model, so treat any trace that
            // does not already overlap the bore as foreign. One that overlaps is
            // this hole's own connection.
            if d <= bore_r + chan_w / 2.0 || d > reach {
                continue;
            }
            let len = ((t.end.x - t.start.x).powi(2) + (t.end.y - t.start.y).powi(2)).sqrt();
            let steps = ((len / keep_out_trace.max(0.05)).ceil() as usize).clamp(1, 64);
            for st in 0..=steps {
                let f = st as f64 / steps as f64;
                let p = Point2::new(
                    t.start.x + (t.end.x - t.start.x) * f,
                    t.start.y + (t.end.y - t.start.y) * f,
                );
                if ((p.x - raw.center.x).powi(2) + (p.y - raw.center.y).powi(2)).sqrt() > reach {
                    continue;
                }
                for (k, dir) in dirs_of[i].iter().enumerate() {
                    if let Some(l) = ray_limit(raw.center, *dir, p, keep_out_trace) {
                        offsets[i][k] = offsets[i][k].min((l - bore_r).max(0.0));
                    }
                }
            }
        }
    }

    // Hole against hole, iterated.
    //
    // A neighbour's mouth is anisotropic too, so there is no fixed share of the
    // gap to assume: it may grow far past half in the very direction that
    // matters. Assuming half is what let two mouths close to 0.016 mm on a
    // 0.30 mm rim in testing. Instead each bore is bounded by the neighbour's
    // *current* widest reach, which is a genuine upper bound on its final size
    // because these offsets only ever shrink. Repeating tightens that bound
    // while never letting a pair overlap at any point in between.
    const CLEARANCE_PASSES: usize = 4;
    for _ in 0..CLEARANCE_PASSES {
        let widest: Vec<f64> =
            offsets.iter().map(|o| o.iter().copied().fold(0.0, f64::max)).collect();
        for i in 0..raws.len() {
            let bore_r = raws[i].w.max(raws[i].h) / 2.0;
            for j in 0..raws.len() {
                if i == j {
                    continue;
                }
                let same_net =
                    matches!((&raws[i].net, &raws[j].net), (Some(a), Some(b)) if a == b);
                if same_net {
                    continue;
                }
                let other_r = raws[j].w.max(raws[j].h) / 2.0;
                let keep_out = other_r + widest[j] + min_rim;
                for k in 0..offsets[i].len() {
                    let dir = dirs_of[i][k];
                    if let Some(l) = ray_limit(raws[i].center, dir, raws[j].center, keep_out) {
                        offsets[i][k] = offsets[i][k].min((l - bore_r).max(0.0));
                    }
                }
            }
        }
    }

    let mut bores = Vec::with_capacity(raws.len());
    let mut limited = 0usize;
    for (i, raw) in raws.iter().enumerate() {
        let widest = offsets[i].iter().copied().fold(0.0, f64::max);
        if widest + 1e-9 < desired_offset {
            limited += 1;
        }
        // Shrinking the mouth shortens the cone at the same wall angle, which
        // lengthens the straight throat rather than making the wall shallower.
        // Keyed off the widest direction so the cone still descends as far as
        // any part of the mouth justifies.
        let cone_depth = (widest * angle.tan()).min(desired_depth);
        bores.push(Bore {
            center: raw.center,
            w: raw.w,
            h: raw.h,
            rotation_deg: raw.rot,
            mouth_offsets: std::mem::take(&mut offsets[i]),
            cone_depth,
            sides: sides_of[i],
        });
    }

    // Every hole gets a footprint of at least `COLLAR_GAP_MM` plus the growth
    // floor, whether or not it has room for a cone — that minimum is what keeps
    // the bore strictly interior to each band and the mesh closed. On holes
    // packed closer than twice that minimum it cannot also honour `min_rim_mm`,
    // and two footprints on different nets end up nearer than intended. Say so:
    // once plated, that is a short, and no amount of shrinking the cone fixes
    // it because the cone is already gone.
    {
        let mut tight = 0usize;
        for (i, a) in bores.iter().enumerate() {
            let ar = a.w.max(a.h) / 2.0 + COLLAR_GAP_MM;
            for (j, b) in bores.iter().enumerate().skip(i + 1) {
                if matches!((&raws[i].net, &raws[j].net), (Some(x), Some(y)) if x == y) {
                    continue;
                }
                let br = b.w.max(b.h) / 2.0 + COLLAR_GAP_MM;
                let d = ((b.center.x - a.center.x).powi(2) + (b.center.y - a.center.y).powi(2))
                    .sqrt();
                if d < ar + br + min_rim {
                    tight += 1;
                }
            }
        }
        if tight > 0 {
            eprintln!(
                "⚠️  {} hole pair(s) on different nets sit closer than {:.2}mm apart even with no \
                 cone at all — inspect those before plating, they can short.",
                tight, min_rim
            );
        }
    }

    if limited > 0 {
        eprintln!(
            "ℹ️  {} of {} cone mouth(s) shrunk to keep {:.2}mm clear of neighbouring copper \
             or the board edge",
            limited,
            bores.len(),
            min_rim
        );
    }
    let fully_straight = bores.iter().filter(|b| b.max_mouth_offset() <= 1e-9).count();
    if fully_straight > 0 {
        eprintln!(
            "   {} hole(s) had no room for a cone at all and stayed straight — those still \
             need an eyelet or a soldered wire stitch.",
            fully_straight
        );
    }

    bores
}


/// Upper bound on slices through the whole substrate.
///





/// Discards polygons too small to be real geometry.
///
/// Differencing two shapes that share part of their boundary — which is the
/// normal case between consecutive bands, since most features are unchanged
/// from one to the next — makes `geo` emit a hairline sliver along every
/// coincident edge. On a real board that is over a hundred spurious pieces per
/// band boundary, each contributing near-zero-area faces whose edges duplicate
/// the walls above and below. They are invisible in the model and ruinous to
/// its topology.
///
/// The cutoff is orders of magnitude below any feature the tool draws (the
/// smallest genuine ledge is a nozzle width across) and orders of magnitude
/// above the numerical noise these slivers are made of.
fn drop_slivers(mp: MultiPolygon) -> MultiPolygon {
    use geo::algorithm::area::Area;
    const MIN_AREA_MM2: f64 = 1e-6;
    MultiPolygon::new(mp.0.into_iter().filter(|p| p.unsigned_area() > MIN_AREA_MM2).collect())
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
    // Snap to the same grid `triangulate_polygon`/`push_ring_snapped` uses for
    // the flat floor/cap faces this wall meets at z_floor and z_opening. Both
    // paths start from the same `geo` boolean-op output, but without matching
    // snapping the flat face's earcut-triangulated vertices and this wall's
    // raw vertices can differ by float noise the sweep algorithm leaves
    // behind — same coordinate, different bits — leaving a non-manifold gap
    // right at the wall/face seam.
    let coords: Vec<Coord> = coords_iter
        .map(|c| Coord { x: snap_coord(c.x), y: snap_coord(c.y) })
        .collect();
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
        union_via_holes(&pcb.vias, config, 16)
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
/// Splits triangle edges that have another vertex lying on them, closing
/// T-junctions.
///
/// Every polygon boolean op is a chance to create one. When two shapes share
/// part of their boundary, `geo` inserts intersection vertices into the result
/// that do not exist in either input's own ring. A wall drawn from the input
/// ring then runs past a vertex that a neighbouring face stops at, so one long
/// edge faces two short ones. No vertex is misplaced and nothing looks wrong in
/// a preview, but the edges cannot pair up, so the surface is formally open —
/// and an open surface is what lets a slicer cork a through-hole or collapse
/// the board into a flat plaque.
///
/// The repair is to give the long edge the missing vertex: find the vertices
/// lying in an edge's interior and fan the owning triangle across them. This is
/// done as a mesh pass rather than by chasing each boolean op, because the
/// defect is a property of the result, not of any one operation.
///
/// Only unpaired edges are considered, so a healthy mesh is left untouched.
/// Splitting can expose further T-junctions, hence the bounded repeat.
fn split_t_junctions(mesh: &mut Mesh3D) {
    use std::collections::{HashMap, HashSet};

    // Same 1e-4 mm grid the rest of the pipeline welds to.
    let quant = |c: f32| (c as f64 * 1e4).round() as i64;
    let qkey = |v: [f32; 3]| (quant(v[0]), quant(v[1]), quant(v[2]));

    const MAX_PASSES: usize = 8;
    // A vertex counts as "on" an edge when it is within slightly more than one
    // weld-grid quantum of it. It has to exceed the grid: every vertex has
    // already been snapped to that grid, so a genuinely collinear point can sit
    // up to half a quantum off the line, and a tighter tolerance would reject
    // exactly the T-junctions this exists to find. Still far below any real
    // feature, so non-collinear vertices are never caught.
    const ON_EDGE_TOL: f64 = 1.5e-4;

    for _ in 0..MAX_PASSES {
        // Unpaired edges are the only ones that can carry a T-junction.
        let mut edge_use: HashMap<((i64, i64, i64), (i64, i64, i64)), usize> = HashMap::new();
        for t in &mesh.triangles {
            for k in 0..3 {
                let (a, b) = (qkey(t.vertices[k]), qkey(t.vertices[(k + 1) % 3]));
                let und = if a <= b { (a, b) } else { (b, a) };
                *edge_use.entry(und).or_default() += 1;
            }
        }
        let open: HashSet<_> =
            edge_use.iter().filter(|(_, &n)| n == 1).map(|(&e, _)| e).collect();
        if open.is_empty() {
            return;
        }

        // Bucket vertices by grid cell so each edge only tests nearby ones.
        // Cell size is generous relative to the edges being repaired.
        const CELL: f64 = 0.5;
        let cell_of = |v: [f32; 3]| {
            (
                (v[0] as f64 / CELL).floor() as i64,
                (v[1] as f64 / CELL).floor() as i64,
                (v[2] as f64 / CELL).floor() as i64,
            )
        };
        let mut grid: HashMap<(i64, i64, i64), Vec<[f32; 3]>> = HashMap::new();
        let mut seen: HashSet<(i64, i64, i64)> = HashSet::new();
        for t in &mesh.triangles {
            for v in t.vertices {
                if seen.insert(qkey(v)) {
                    grid.entry(cell_of(v)).or_default().push(v);
                }
            }
        }

        // Vertices strictly inside edge a→b, ordered along it.
        let splits_on = |a: [f32; 3], b: [f32; 3]| -> Vec<[f32; 3]> {
            let d = [
                b[0] as f64 - a[0] as f64,
                b[1] as f64 - a[1] as f64,
                b[2] as f64 - a[2] as f64,
            ];
            let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if len2 <= 0.0 {
                return Vec::new();
            }
            let (lo, hi) = (cell_of(a), cell_of(b));
            let mut found: Vec<(f64, [f32; 3])> = Vec::new();
            for cx in lo.0.min(hi.0) - 1..=lo.0.max(hi.0) + 1 {
                for cy in lo.1.min(hi.1) - 1..=lo.1.max(hi.1) + 1 {
                    for cz in lo.2.min(hi.2) - 1..=lo.2.max(hi.2) + 1 {
                        let Some(cands) = grid.get(&(cx, cy, cz)) else { continue };
                        for &v in cands {
                            if qkey(v) == qkey(a) || qkey(v) == qkey(b) {
                                continue;
                            }
                            let ap = [
                                v[0] as f64 - a[0] as f64,
                                v[1] as f64 - a[1] as f64,
                                v[2] as f64 - a[2] as f64,
                            ];
                            let t = (ap[0] * d[0] + ap[1] * d[1] + ap[2] * d[2]) / len2;
                            if !(0.0..=1.0).contains(&t) {
                                continue;
                            }
                            // Perpendicular distance from the segment.
                            let perp = [
                                ap[0] - t * d[0],
                                ap[1] - t * d[1],
                                ap[2] - t * d[2],
                            ];
                            let dist2 = perp[0] * perp[0] + perp[1] * perp[1] + perp[2] * perp[2];
                            if dist2 <= ON_EDGE_TOL * ON_EDGE_TOL {
                                found.push((t, v));
                            }
                        }
                    }
                }
            }
            found.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            found.dedup_by_key(|(_, v)| qkey(*v));
            found.into_iter().map(|(_, v)| v).collect()
        };

        let mut out: Vec<Triangle3D> = Vec::with_capacity(mesh.triangles.len());
        let mut split_any = false;

        for t in &mesh.triangles {
            // Repair one edge per triangle per pass; a triangle needing more
            // than one is picked up by a later pass.
            let mut done = false;
            for k in 0..3 {
                let (a, b, c) =
                    (t.vertices[k], t.vertices[(k + 1) % 3], t.vertices[(k + 2) % 3]);
                let (qa, qb) = (qkey(a), qkey(b));
                let und = if qa <= qb { (qa, qb) } else { (qb, qa) };
                if !open.contains(&und) {
                    continue;
                }
                let pts = splits_on(a, b);
                if pts.is_empty() {
                    continue;
                }
                // Fan the opposite corner across the subdivided edge. The
                // pieces are coplanar with their parent, so they keep its
                // normal exactly; winding is settled by the caller anyway.
                let mut prev = a;
                for p in pts {
                    out.push(Triangle3D { normal: t.normal, vertices: [prev, p, c] });
                    prev = p;
                }
                out.push(Triangle3D { normal: t.normal, vertices: [prev, b, c] });
                split_any = true;
                done = true;
                break;
            }
            if !done {
                out.push(t.clone());
            }
        }

        mesh.triangles = out;
        mesh.triangles.retain(|t| {
            qkey(t.vertices[0]) != qkey(t.vertices[1])
                && qkey(t.vertices[1]) != qkey(t.vertices[2])
                && qkey(t.vertices[2]) != qkey(t.vertices[0])
        });

        if !split_any {
            return;
        }
    }
}

/// Caps small leftover holes in the surface by fanning each boundary loop.
///
/// Runs after `split_t_junctions`, which handles the common case. What survives
/// is usually a sliver a few hundredths of a millimetre across, where earcut
/// dropped a degenerate triangle in a very thin region and left a genuine gap
/// with no vertex to snap to. Too small to matter geometrically, big enough to
/// leave the solid open.
///
/// Only short loops are capped. A large boundary loop means something
/// substantial is missing — a whole face, say — and quietly patching that over
/// would hide a real bug behind a plausible-looking mesh. Those are left for
/// `validate_mesh` to report.
///
/// Winding is not chosen carefully here: once the loop is closed its edges pair
/// up, so the orientation flood-fill in `make_outward_consistent` settles it.
fn fill_boundary_loops(mesh: &mut Mesh3D) {
    use std::collections::HashMap;

    // Above this, a hole is a symptom rather than a sliver.
    const MAX_LOOP_EDGES: usize = 32;

    let quant = |c: f32| (c as f64 * 1e4).round() as i64;
    let qkey = |v: [f32; 3]| (quant(v[0]), quant(v[1]), quant(v[2]));

    let mut count: HashMap<((i64, i64, i64), (i64, i64, i64)), usize> = HashMap::new();
    for t in &mesh.triangles {
        for k in 0..3 {
            let (a, b) = (qkey(t.vertices[k]), qkey(t.vertices[(k + 1) % 3]));
            let und = if a <= b { (a, b) } else { (b, a) };
            *count.entry(und).or_default() += 1;
        }
    }

    // Chain the boundary edges *undirected*. Orientation has not been settled
    // yet at this point in the pipeline — that happens further down — so the
    // directions these edges happen to carry are arbitrary and neighbouring
    // ones may disagree. Following them as directed simply fails to close.
    let mut adj: HashMap<(i64, i64, i64), Vec<(i64, i64, i64)>> = HashMap::new();
    let mut coord: HashMap<(i64, i64, i64), [f32; 3]> = HashMap::new();
    for t in &mesh.triangles {
        for k in 0..3 {
            let (va, vb) = (t.vertices[k], t.vertices[(k + 1) % 3]);
            let (a, b) = (qkey(va), qkey(vb));
            let und = if a <= b { (a, b) } else { (b, a) };
            if count.get(&und) == Some(&1) {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
                coord.insert(a, va);
                coord.insert(b, vb);
            }
        }
    }
    if adj.is_empty() {
        return;
    }

    let mut used: std::collections::HashSet<((i64, i64, i64), (i64, i64, i64))> = Default::default();
    let mut caps: Vec<Vec<[f32; 3]>> = Vec::new();
    // Sorted, not in hash order. The walk below consumes edges greedily, so the
    // order it visits vertices decides how the boundary graph gets split into
    // loops — and `HashMap` iteration order is randomised per process, which
    // made the whole model differ from run to run. Neighbours are sorted for
    // the same reason.
    let mut starts: Vec<_> = adj.keys().copied().collect();
    starts.sort_unstable();
    for nbrs in adj.values_mut() {
        nbrs.sort_unstable();
    }

    for start in starts {
        let mut ring = vec![start];
        let mut cur = start;
        let mut closed = false;
        for _ in 0..=MAX_LOOP_EDGES {
            let Some(nbrs) = adj.get(&cur) else { break };
            // Take any edge out of `cur` not already consumed.
            let Some(&nxt) = nbrs.iter().find(|&&n| {
                let e = if cur <= n { (cur, n) } else { (n, cur) };
                !used.contains(&e)
            }) else {
                break;
            };
            used.insert(if cur <= nxt { (cur, nxt) } else { (nxt, cur) });
            if nxt == start {
                closed = true;
                break;
            }
            ring.push(nxt);
            cur = nxt;
        }
        if closed && ring.len() >= 3 {
            caps.push(ring.iter().filter_map(|k| coord.get(k).copied()).collect());
        }
    }

    // Fan each loop, but never at the cost of a worse defect. Where the
    // boundary graph branches, the greedy walk above can return a loop that
    // cuts across an edge the mesh already has; fanning it would give that edge
    // a third face. A non-manifold edge is harder for a slicer to interpret
    // than the small gap it replaces, so a triangle that would create one is
    // skipped and the gap left for `validate_mesh` to report.
    for loop_pts in caps {
        let anchor = loop_pts[0];
        for w in loop_pts[1..].windows(2) {
            let verts = [anchor, w[0], w[1]];
            let keys = [qkey(verts[0]), qkey(verts[1]), qkey(verts[2])];
            if keys[0] == keys[1] || keys[1] == keys[2] || keys[2] == keys[0] {
                continue;
            }
            let tri_edges: Vec<_> = (0..3)
                .map(|k| {
                    let (a, b) = (keys[k], keys[(k + 1) % 3]);
                    if a <= b { (a, b) } else { (b, a) }
                })
                .collect();
            if tri_edges.iter().any(|e| count.get(e).copied().unwrap_or(0) >= 2) {
                continue;
            }
            for e in tri_edges {
                *count.entry(e).or_default() += 1;
            }
            mesh.triangles.push(Triangle3D { normal: [0.0, 0.0, 1.0], vertices: verts });
        }
    }
}

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

    // Weld near-coincident vertices to one canonical position first. Faces and
    // hole walls are built independently (top/bottom faces go through several
    // `geo` boolean ops — difference/intersection — that regenerate coordinates
    // with tiny float drift, while hole-wall triangles read the ring vertices
    // directly), so geometrically-identical points routinely differ by a few
    // ULPs. Bit-exact matching then treats them as distinct vertices, leaving
    // real micro-gaps around every hole that a slicer can trip on. Snapping to
    // a 1e-4 mm grid (far below manufacturing precision, well above the float
    // noise from a few chained boolean ops) merges those without touching
    // genuine geometry.
    let quant = |c: f32| (c as f64 * 1e4).round() as i64;
    let qkey = |v: [f32; 3]| (quant(v[0]), quant(v[1]), quant(v[2]));
    let mut canonical: HashMap<(i64, i64, i64), [f32; 3]> = HashMap::new();
    for t in mesh.triangles.iter() {
        for v in t.vertices {
            canonical.entry(qkey(v)).or_insert(v);
        }
    }
    for t in mesh.triangles.iter_mut() {
        for v in t.vertices.iter_mut() {
            *v = canonical[&qkey(*v)];
        }
    }
    // Re-drop any triangle that welding collapsed into a degenerate sliver.
    mesh.triangles
        .retain(|t| t.vertices[0] != t.vertices[1] && t.vertices[1] != t.vertices[2] && t.vertices[2] != t.vertices[0]);
    let n = mesh.triangles.len();
    if n == 0 {
        return;
    }

    // Close T-junctions before deciding orientation. Welding alone cannot fix
    // them: the vertices already coincide, it is the *edges* that disagree.
    // Called from here rather than from each generator so it cannot be
    // forgotten when a new one is added.
    split_t_junctions(mesh);
    // Then cap whatever slivers survive, so the solid is genuinely closed.
    fill_boundary_loops(mesh);
    // The repair adds and removes triangles, so every index-keyed structure
    // below must be sized from the new count, not the pre-repair one.
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

    // Flood-fill a flip flag across every connected component, tracking each
    // component's member triangles separately — components are NOT necessarily
    // one single shell (hole-wall tubes, floating copper islands, etc. can end
    // up vertex-disconnected from the main shell), so each needs its own
    // independent outward-orientation decision below rather than one global one.
    let mut flip = vec![false; n];
    let mut seen = vec![false; n];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut component = vec![start];
        while let Some(t) = stack.pop() {
            for &(nb, consistent) in &adj[t] {
                if !seen[nb] {
                    seen[nb] = true;
                    flip[nb] = if consistent { flip[t] } else { !flip[t] };
                    stack.push(nb);
                    component.push(nb);
                }
            }
        }
        components.push(component);
    }
    for (ti, t) in mesh.triangles.iter_mut().enumerate() {
        if flip[ti] {
            t.vertices.swap(1, 2);
        }
    }

    // Orient each component outward independently: a closed surface with
    // outward normals encloses positive volume.
    let signed_vol = |t: &Triangle3D| -> f64 {
        let [a, b, c] = t.vertices;
        (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0])) as f64
    };
    for component in &components {
        let vol: f64 = component.iter().map(|&ti| signed_vol(&mesh.triangles[ti])).sum();
        if vol < 0.0 {
            for &ti in component {
                mesh.triangles[ti].vertices.swap(1, 2);
            }
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


// ---------------------------------------------------------------------------
// Mesh validation
// ---------------------------------------------------------------------------

/// Structural health of a generated mesh.
///
/// Exists because the failure modes here are silent in the preview and only
/// show up in the slicer — a corked through-hole, or a "flat blank plaque"
/// where the walls went missing and only the faces survived. Both come from
/// the surface not being closed, which is cheap to detect directly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshReport {
    pub triangles: usize,
    pub vertices: usize,
    /// Edges used by exactly one triangle — literal holes in the surface.
    /// Any non-zero count means the solid is open and the slicer is free to
    /// interpret the interior however it likes.
    pub boundary_edges: usize,
    /// Edges shared by three or more triangles. The surface self-touches, and
    /// "inside" stops being well defined.
    pub nonmanifold_edges: usize,
    /// Edges whose two triangles traverse them the same way, i.e. one of the
    /// pair is wound backwards relative to its neighbour.
    pub flipped_edges: usize,
    /// Zero-area triangles.
    pub degenerate_triangles: usize,
    /// Connected components of the surface.
    pub shells: usize,
    /// Number of through-tunnels implied by the topology, summed over shells.
    /// For a plate this should equal the number of through-holes: a hole the
    /// slicer will cork usually shows up here as a missing tunnel long before
    /// anyone opens the STL.
    pub genus: i64,
    /// Enclosed signed volume in mm³. Must be positive; ~0 means the mesh
    /// encloses nothing (the blank-plaque case).
    pub volume_mm3: f64,
    /// Midpoints of up to a handful of open edges, in model coordinates.
    /// A gap is far easier to diagnose when you can go and look at it.
    pub boundary_samples: Vec<[f32; 3]>,
    /// Same, for non-manifold edges.
    pub nonmanifold_samples: Vec<[f32; 3]>,
}

impl MeshReport {
    /// True when the mesh is a closed, consistently-wound, positive-volume
    /// solid — the properties a slicer needs in order to produce the shape
    /// that the preview showed.
    pub fn is_watertight(&self) -> bool {
        self.boundary_edges == 0
            && self.nonmanifold_edges == 0
            && self.flipped_edges == 0
            && self.volume_mm3 > 0.0
    }

    /// One-line human summary.
    pub fn summary(&self) -> String {
        format!(
            "{} triangles, {} vertices, {} shells, genus {}, {:.1} mm³{}",
            self.triangles,
            self.vertices,
            self.shells,
            self.genus,
            self.volume_mm3,
            if self.is_watertight() { ", watertight" } else { ", NOT watertight" }
        )
    }

    /// Multi-line description of everything wrong, or `None` if the mesh is
    /// sound. Written to be actionable rather than merely alarming.
    pub fn problems(&self) -> Option<String> {
        let mut out = Vec::new();
        if self.boundary_edges > 0 {
            let mut msg = format!(
                "{} open edge(s): the surface has gaps, so the slicer may fill holes solid or \
                 produce a flat plaque",
                self.boundary_edges
            );
            for s in &self.boundary_samples {
                msg.push_str(&format!("\n    near ({:.3}, {:.3}, {:.3})", s[0], s[1], s[2]));
            }
            out.push(msg);
        }
        if self.nonmanifold_edges > 0 {
            let mut msg = format!(
                "{} non-manifold edge(s): three or more faces meet, so 'inside' is ambiguous",
                self.nonmanifold_edges
            );
            for s in &self.nonmanifold_samples {
                msg.push_str(&format!("\n    near ({:.3}, {:.3}, {:.3})", s[0], s[1], s[2]));
            }
            out.push(msg);
        }
        if self.flipped_edges > 0 {
            out.push(format!(
                "{} inconsistently-wound edge(s): some faces point the wrong way",
                self.flipped_edges
            ));
        }
        if self.volume_mm3 <= 0.0 {
            out.push(format!(
                "enclosed volume is {:.3} mm³: the mesh encloses no solid",
                self.volume_mm3
            ));
        }
        if out.is_empty() {
            None
        } else {
            Some(out.join("\n"))
        }
    }
}

/// Inspects a mesh for the structural defects that make slicers misbehave.
///
/// Vertices are matched on the same 1e-4 mm grid `make_outward_consistent`
/// welds to, so this measures the mesh as a slicer would see it rather than
/// flagging float noise.
pub fn validate_mesh(mesh: &Mesh3D) -> MeshReport {
    use std::collections::HashMap;

    let quant = |c: f32| (c as f64 * 1e4).round() as i64;
    let qkey = |v: [f32; 3]| (quant(v[0]), quant(v[1]), quant(v[2]));

    let mut report = MeshReport {
        triangles: mesh.triangles.len(),
        ..Default::default()
    };
    if mesh.triangles.is_empty() {
        return report;
    }

    // Index vertices on the weld grid.
    let mut vert_id: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut tri_ids: Vec<[usize; 3]> = Vec::with_capacity(mesh.triangles.len());
    for t in &mesh.triangles {
        let mut ids = [0usize; 3];
        for (k, v) in t.vertices.iter().enumerate() {
            let next = vert_id.len();
            ids[k] = *vert_id.entry(qkey(*v)).or_insert(next);
        }
        tri_ids.push(ids);
    }
    report.vertices = vert_id.len();

    // A triangle with a repeated welded vertex has no area.
    report.degenerate_triangles = tri_ids
        .iter()
        .filter(|t| t[0] == t[1] || t[1] == t[2] || t[2] == t[0])
        .count();

    // Undirected edge → directed traversals, for manifoldness and winding.
    let mut edges: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for (ti, t) in tri_ids.iter().enumerate() {
        if t[0] == t[1] || t[1] == t[2] || t[2] == t[0] {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let und = if a <= b { (a, b) } else { (b, a) };
            edges.entry(und).or_default().push((ti, a));
        }
    }

    // Reverse index so boundary edges can be reported in model coordinates.
    let mut pos: Vec<[f32; 3]> = vec![[0.0; 3]; vert_id.len()];
    for t in &mesh.triangles {
        for v in t.vertices {
            pos[vert_id[&qkey(v)]] = v;
        }
    }
    const MAX_BOUNDARY_SAMPLES: usize = 8;

    for ((a, b), inc) in &edges {
        match inc.len() {
            1 => {
                report.boundary_edges += 1;
                if report.boundary_samples.len() < MAX_BOUNDARY_SAMPLES {
                    let (p, q) = (pos[*a], pos[*b]);
                    report.boundary_samples.push([
                        (p[0] + q[0]) / 2.0,
                        (p[1] + q[1]) / 2.0,
                        (p[2] + q[2]) / 2.0,
                    ]);
                }
            }
            // Consistent iff the two traversals start at opposite ends.
            2 => {
                if inc[0].1 == inc[1].1 {
                    report.flipped_edges += 1;
                }
            }
            _ => {
                report.nonmanifold_edges += 1;
                if report.nonmanifold_samples.len() < MAX_BOUNDARY_SAMPLES {
                    let (p, q) = (pos[*a], pos[*b]);
                    report.nonmanifold_samples.push([
                        (p[0] + q[0]) / 2.0,
                        (p[1] + q[1]) / 2.0,
                        (p[2] + q[2]) / 2.0,
                    ]);
                }
            }
        }
    }

    // Connected components over shared edges.
    let n = tri_ids.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        let mut x = x;
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for inc in edges.values() {
        for w in inc.windows(2) {
            let (ra, rb) = (find(&mut parent, w[0].0), find(&mut parent, w[1].0));
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut roots: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for i in 0..n {
        roots.insert(find(&mut parent, i));
    }
    report.shells = roots.len();

    // Euler characteristic V − E + F = 2 − 2g per closed shell. Summed over
    // shells this gives the total tunnel count, which is what tells us the
    // through-holes actually made it into the surface. Only meaningful when
    // the mesh is closed, so leave genus at 0 otherwise rather than reporting
    // a number derived from a broken surface.
    if report.boundary_edges == 0 && report.nonmanifold_edges == 0 {
        let v = report.vertices as i64;
        let e = edges.len() as i64;
        let f = (n - report.degenerate_triangles) as i64;
        let chi = v - e + f;
        report.genus = (2 * report.shells as i64 - chi) / 2;
    }

    // Signed volume via the divergence theorem (sum of tetrahedra to origin).
    let mut vol = 0.0f64;
    for t in &mesh.triangles {
        let [a, b, c] = t.vertices;
        let (ax, ay, az) = (a[0] as f64, a[1] as f64, a[2] as f64);
        let (bx, by, bz) = (b[0] as f64, b[1] as f64, b[2] as f64);
        let (cx, cy, cz) = (c[0] as f64, c[1] as f64, c[2] as f64);
        vol += (ax * (by * cz - bz * cy) - ay * (bx * cz - bz * cx) + az * (bx * cy - by * cx))
            / 6.0;
    }
    report.volume_mm3 = vol;

    report
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcb::BoardOutline;

    fn pt(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn trace(layer: CopperLayer, a: (f64, f64), b: (f64, f64)) -> Trace {
        Trace { layer, start: pt(a.0, a.1), end: pt(b.0, b.1), width: 0.25 }
    }

    fn tht_pad(x: f64, y: f64, drill: f64) -> Pad {
        Pad {
            center: pt(x, y),
            drill,
            drill_h: drill,
            number: "1".into(),
            net_name: Some("N1".into()),
            width: 1.6,
            height: 1.6,
            shape: PadShape::Circle,
            rotation_deg: 0.0,
            on_fcu: true,
            on_bcu: true,
        }
    }

    /// A small two-layer board: a rectangular outline, one trace per layer, two
    /// through-hole pads and one via. Deliberately keeps the features apart so
    /// the hole count is unambiguous for the genus check.
    fn test_board() -> PcbData {
        PcbData {
            outline: Some(BoardOutline::new(vec![
                pt(0.0, 0.0),
                pt(30.0, 0.0),
                pt(30.0, 20.0),
                pt(0.0, 20.0),
            ])),
            traces_fcu: vec![trace(CopperLayer::FCu, (5.0, 5.0), (25.0, 5.0))],
            traces_bcu: vec![trace(CopperLayer::BCu, (5.0, 15.0), (25.0, 15.0))],
            vias: vec![Via { center: pt(15.0, 10.0), drill: 0.4, net_name: Some("N1".into()) }],
            pads: vec![tht_pad(5.0, 5.0, 1.0), tht_pad(25.0, 5.0, 1.0)],
            ..Default::default()
        }
    }

    fn cfg(profile: ChannelProfile) -> Config {
        Config {
            channel_profile: profile,
            // Deep enough relative to the substrate to make the taper obvious,
            // shallow enough not to trip the half-thickness warning.
            channel_width_mm: 1.2,
            channel_depth_mm: 0.6,
            substrate_thickness_mm: 2.2,
            generate_pad_lands: false,
            ..Config::default()
        }
    }

    /// The number of distinct through-holes in `test_board`: two pad drills plus
    /// one via, none of them overlapping.
    const TEST_BOARD_HOLES: i64 = 3;

    fn assert_sound(mesh: &Mesh3D, what: &str) -> MeshReport {
        let report = validate_mesh(mesh);
        assert!(
            report.is_watertight(),
            "{what}: mesh is not watertight — {}\n{}",
            report.summary(),
            report.problems().unwrap_or_default()
        );
        assert_eq!(
            report.genus, TEST_BOARD_HOLES,
            "{what}: expected {TEST_BOARD_HOLES} through-holes in the topology, got genus {} \
             ({}). A missing tunnel here is a hole the slicer will cork.",
            report.genus,
            report.summary()
        );
        report
    }




    #[test]
    fn rect_profile_produces_a_watertight_solid_with_every_hole_open() {
        let mesh = generate_model(&test_board(), &cfg(ChannelProfile::Rect)).expect("model builds");
        assert_sound(&mesh, "rect");
    }

    #[test]
    fn trapezoid_profile_produces_a_watertight_solid_with_every_hole_open() {
        let mesh =
            generate_model(&test_board(), &cfg(ChannelProfile::Trapezoid)).expect("model builds");
        assert_sound(&mesh, "trapezoid");
    }

    #[test]
    fn vee_profile_produces_a_watertight_solid_with_every_hole_open() {
        let mesh = generate_model(&test_board(), &cfg(ChannelProfile::Vee)).expect("model builds");
        assert_sound(&mesh, "vee");
    }

    #[test]
    fn tapered_profiles_remove_less_material_than_a_rectangular_one() {
        // Same opening width and depth, narrower floor ⇒ strictly more substrate
        // left behind. This is the cheap proxy for "the taper actually happened":
        // a silently-ignored profile setting would give identical volumes.
        let rect = validate_mesh(&generate_model(&test_board(), &cfg(ChannelProfile::Rect)).unwrap());
        let trap_cfg =
            Config { channel_floor_width_mm: 0.8, ..cfg(ChannelProfile::Trapezoid) };
        let trap = validate_mesh(&generate_model(&test_board(), &trap_cfg).unwrap());
        let vee = validate_mesh(&generate_model(&test_board(), &cfg(ChannelProfile::Vee)).unwrap());

        assert!(
            trap.volume_mm3 > rect.volume_mm3,
            "trapezoid ({:.3}) should leave more material than rect ({:.3})",
            trap.volume_mm3,
            rect.volume_mm3
        );
        assert!(
            vee.volume_mm3 > trap.volume_mm3,
            "vee ({:.3}) should leave more material than trapezoid ({:.3})",
            vee.volume_mm3,
            trap.volume_mm3
        );
    }

    #[test]
    fn pad_lands_stay_watertight_when_merged_into_a_tapered_network() {
        // Lands are full-size in every band while the traces taper, so the
        // land/trace boundary differs from band to band — the most likely place
        // for the ledge construction to leave a crack.
        let config = Config { generate_pad_lands: true, ..cfg(ChannelProfile::Vee) };
        let mesh = generate_model(&test_board(), &config).expect("model builds");
        let report = validate_mesh(&mesh);
        assert!(
            report.is_watertight(),
            "tapered network with pad lands is not watertight — {}\n{}",
            report.summary(),
            report.problems().unwrap_or_default()
        );
    }

    #[test]
    fn validator_flags_an_open_surface() {
        // Guard against the validator itself silently passing everything: strip
        // one triangle and it must notice the resulting gap.
        let mut mesh = generate_model(&test_board(), &cfg(ChannelProfile::Rect)).unwrap();
        mesh.triangles.pop();
        let report = validate_mesh(&mesh);
        assert!(!report.is_watertight(), "removing a face must break watertightness");
        assert_eq!(report.boundary_edges, 3, "a missing triangle leaves three open edges");
    }

    /// A pad drill and channel width whose collar radius would land exactly on
    /// the channel's half-width. Found on a real board: a 0.9 mm drill gives a
    /// 0.45 mm bore radius, and with a 0.15 mm collar gap that is 0.60 mm —
    /// precisely half of a 1.2 mm channel. The collar circle then coincided
    /// with the trace capsule's end cap and left the mesh non-manifold.
    #[test]
    fn collar_radius_does_not_coincide_with_the_channel_half_width() {
        let mut pcb = test_board();
        pcb.pads = vec![tht_pad(5.0, 5.0, 0.9), tht_pad(25.0, 5.0, 0.9)];
        for profile in [ChannelProfile::Rect, ChannelProfile::Trapezoid, ChannelProfile::Vee] {
            let config = Config { channel_width_mm: 1.2, ..cfg(profile) };
            let mesh = generate_model(&pcb, &config).expect("model builds");
            let report = validate_mesh(&mesh);
            assert!(
                report.is_watertight(),
                "{profile}: 0.9mm drill in a 1.2mm channel broke the mesh — {}\n{}",
                report.summary(),
                report.problems().unwrap_or_default()
            );
        }
    }

    /// Regression for the case that produced no collars at all: the rectangular
    /// profile with pad lands turned off. Nothing then kept the channel
    /// boundary off the bore, and the barrel wall had nothing to pair with.
    #[test]
    fn rect_profile_without_pad_lands_is_still_watertight() {
        let config = Config { generate_pad_lands: false, ..cfg(ChannelProfile::Rect) };
        let mesh = generate_model(&test_board(), &config).expect("model builds");
        let report = validate_mesh(&mesh);
        assert!(
            report.is_watertight(),
            "rect without pad lands is not watertight — {}\n{}",
            report.summary(),
            report.problems().unwrap_or_default()
        );
    }

    /// The same board must produce the same mesh every time. It did not before
    /// the `geo` upgrade: the boolean-op sweep panicked intermittently, and the
    /// panic-safe wrapper silently kept the uncut geometry, so runs differed by
    /// hundreds of triangles — including runs where the top face never got its
    /// channels cut at all.
    ///
    /// Note what this test cannot see. Rust seeds `HashMap` once per *process*,
    /// so repeating the work here reuses the same seed; a second bug where mesh
    /// repair walked a `HashMap` in hash order stayed invisible to a test
    /// shaped like this one and only showed up running the binary repeatedly.
    /// Iteration order over a hash container therefore has to be sorted at the
    /// source rather than relied on being caught here.
    #[test]
    fn model_generation_is_deterministic() {
        let pcb = test_board();
        for style in [ViaStyle::Straight, ViaStyle::Cone] {
            let config = Config { via_style: style, ..cfg(ChannelProfile::Vee) };
            let first = generate_model(&pcb, &config).expect("model builds");
            for _ in 0..3 {
                let again = generate_model(&pcb, &config).expect("model builds");
                assert_eq!(
                    first.triangles.len(),
                    again.triangles.len(),
                    "{style}: triangle count varied between identical runs"
                );
            }
        }
    }

    /// Cones are built from the channel band stack rather than a separate sweep
    /// of the whole board. Every profile has to survive that, including `rect`,
    /// which has no taper of its own and so only gets a multi-band stack
    /// *because* cones need one.
    #[test]
    fn cone_barrels_produce_a_watertight_solid() {
        for profile in [ChannelProfile::Rect, ChannelProfile::Trapezoid, ChannelProfile::Vee] {
            let config = Config { via_style: ViaStyle::Cone, ..cfg(profile) };
            let mesh = generate_model(&test_board(), &config).expect("model builds");
            let report = validate_mesh(&mesh);
            assert!(
                report.is_watertight(),
                "cone + {profile}: not watertight — {}\n{}",
                report.summary(),
                report.problems().unwrap_or_default()
            );
            assert_eq!(
                report.genus, TEST_BOARD_HOLES,
                "cone + {profile}: expected {TEST_BOARD_HOLES} tunnels, got genus {} ({})",
                report.genus,
                report.summary()
            );
        }
    }

    /// The defect that made cones unusable: clearance limiting flattens most
    /// mouths, and a flattened cone's footprint would repeat identically band
    /// after band. Two levels sharing a boundary make the ledge between them a
    /// zero-width spur whose two sides each bound a face — four faces on one
    /// edge. `MIN_BAND_GROWTH_MM` is what keeps every feature strictly growing.
    #[test]
    fn a_clearance_flattened_cone_still_grows_between_bands() {
        let bore = Bore {
            center: pt(0.0, 0.0),
            w: 1.0,
            h: 1.0,
            rotation_deg: 0.0,
            // Shrunk to nothing in every direction — no cone left to taper.
            mouth_offsets: vec![0.0; 16],
            cone_depth: 0.0,
            sides: 16,
        };
        let offs: Vec<f64> = (0..6).map(|b| bore.offset_at(0, 0.0, b)).collect();
        for w in offs.windows(2) {
            assert!(
                w[1] > w[0],
                "footprint must grow strictly between bands even with no cone: {offs:?}"
            );
        }
        assert!(
            offs.last().unwrap() - offs.first().unwrap() < 0.3,
            "the growth floor must stay far below min_rim_mm so it cannot close a clearance gap"
        );
    }

    /// The bug that made anisotropy actively harmful: a neighbour to the east
    /// must not constrain the mouth to the west. Both roots of the ray/disc
    /// intersection are behind the origin in that case, and clamping them to
    /// zero — rather than rejecting them — zeroed the mouth in every direction
    /// and left 36 holes with no cone at all.
    #[test]
    fn a_neighbour_only_constrains_the_direction_it_lies_in() {
        let origin = pt(0.0, 0.0);
        let east = pt(2.0, 0.0);
        let toward = ray_limit(origin, (1.0, 0.0), east, 0.5);
        let away = ray_limit(origin, (-1.0, 0.0), east, 0.5);
        let across = ray_limit(origin, (0.0, 1.0), east, 0.5);

        assert!(matches!(toward, Some(l) if (l - 1.5).abs() < 1e-9), "toward: {toward:?}");
        assert_eq!(away, None, "a neighbour behind must not constrain this direction");
        assert_eq!(across, None, "a neighbour off to the side must not constrain either");
        // Already overlapping leaves no room at all.
        assert_eq!(ray_limit(origin, (1.0, 0.0), pt(0.2, 0.0), 0.5), Some(0.0));
    }

    /// Two holes on different nets, close on one axis and open on the other.
    /// The mouths must go oval — reaching full size away from each other — not
    /// shrink uniformly, which is what wasted most of the available area.
    #[test]
    fn a_crowded_mouth_goes_oval_rather_than_shrinking_all_round() {
        let mut pcb = test_board();
        // 1.6mm apart on x, nothing near on y, and deliberately different nets.
        let mut a = tht_pad(10.0, 10.0, 1.0);
        a.net_name = Some("NET_A".into());
        let mut b = tht_pad(11.6, 10.0, 1.0);
        b.net_name = Some("NET_B".into());
        pcb.pads = vec![a, b];
        pcb.traces_fcu.clear();
        pcb.traces_bcu.clear();
        pcb.vias.clear();

        let config = Config { via_style: ViaStyle::Cone, ..cfg(ChannelProfile::Vee) };
        let outline = pcb.outline.as_ref().unwrap();
        let bores = plan_bores(&pcb, &config, outline);
        assert_eq!(bores.len(), 2);

        for bore in &bores {
            let min = bore.mouth_offsets.iter().copied().fold(f64::INFINITY, f64::min);
            let max = bore.max_mouth_offset();
            assert!(
                max - min > 0.05,
                "mouth should be oval, not uniform: min={min:.3} max={max:.3}"
            );
        }

        // And the result must still be a closed solid.
        let mesh = generate_model(&pcb, &config).expect("model builds");
        let report = validate_mesh(&mesh);
        assert!(
            report.is_watertight(),
            "crowded cone mouths broke the mesh — {}\n{}",
            report.summary(),
            report.problems().unwrap_or_default()
        );
    }

    #[test]
    fn channel_floor_width_is_clamped_into_a_printable_range() {
        let base = cfg(ChannelProfile::Trapezoid);
        // Rect ignores the floor setting entirely.
        assert_eq!(channel_floor_width(&cfg(ChannelProfile::Rect)), base.channel_width_mm);
        // A floor wider than the opening would invert the taper.
        let wide = Config { channel_floor_width_mm: 99.0, ..base.clone() };
        assert_eq!(channel_floor_width(&wide), wide.channel_width_mm);
        // A sub-nozzle trapezoid floor is raised to one printable track.
        let thin = Config { channel_floor_width_mm: 0.01, ..base.clone() };
        assert_eq!(channel_floor_width(&thin), MIN_TAPER_FLOOR_MM);
    }

    /// The two profiles used to be byte-identical at stock settings, because
    /// vee clamped up to the same 0.4 mm that trapezoid defaults to. Choosing
    /// between the flags did nothing.
    #[test]
    fn vee_actually_converges_and_differs_from_trapezoid() {
        let vee = cfg(ChannelProfile::Vee);
        let trap = cfg(ChannelProfile::Trapezoid);
        assert_eq!(channel_floor_width(&vee), VEE_APEX_WIDTH_MM);
        assert!(
            channel_floor_width(&vee) < channel_floor_width(&trap),
            "a vee must converge further than a trapezoid's flat floor"
        );
        // `channel_floor_width_mm` is a trapezoid setting; a vee ignores it.
        let vee_tweaked = Config { channel_floor_width_mm: 0.6, ..vee.clone() };
        assert_eq!(channel_floor_width(&vee_tweaked), VEE_APEX_WIDTH_MM);

        let m_vee = generate_model(&test_board(), &vee).expect("vee builds");
        let m_trap = generate_model(&test_board(), &trap).expect("trapezoid builds");
        let (r_vee, r_trap) = (validate_mesh(&m_vee), validate_mesh(&m_trap));
        assert!(r_vee.is_watertight(), "vee not watertight — {}", r_vee.summary());
        assert!(
            r_vee.volume_mm3 > r_trap.volume_mm3,
            "a converging vee removes less material than a flat-floored trapezoid \
             (vee {:.3} vs trapezoid {:.3})",
            r_vee.volume_mm3,
            r_trap.volume_mm3
        );
    }

    /// A vee narrower than its own apex width must not invert the taper.
    #[test]
    fn vee_apex_never_exceeds_the_opening() {
        let narrow = Config { channel_width_mm: 0.05, ..cfg(ChannelProfile::Vee) };
        assert_eq!(channel_floor_width(&narrow), 0.05);
    }

    #[test]
    fn taper_band_count_is_bounded() {
        assert_eq!(taper_band_count(0.6, 0.2), 3);
        assert_eq!(taper_band_count(0.0, 0.2), 1, "no depth means no taper");
        assert_eq!(taper_band_count(0.6, 0.0), 1, "no slice height means no taper");
        assert_eq!(
            taper_band_count(100.0, 0.01),
            MAX_TAPER_BANDS,
            "band count must stay bounded — each band is a boolean op"
        );
    }
}
