// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! KiCad PCB file parser.
//!
//! This module walks the S-expression tree produced by the S-expression parser
//! and extracts meaningful PCB design data (traces, vias, pads, outline).
//!
//! Key design decisions:
//! - Only traces on F.Cu and B.Cu copper layers are extracted (internal copper ignored)
//! - Only through-hole pads are extracted (SMD pads without drills are skipped)
//! - Y coordinates are negated to convert from KiCad's Y-down convention to standard Y-up
//! - Board outline from Edge.Cuts layer segments are sorted and chained into a closed polygon

use crate::pcb::*;
use crate::parser::sexp::SexpNode;
use anyhow::{anyhow, Result};

/// Walks the KiCad S-expression tree and extracts PCB design data.
///
/// Expects the tree to be the parsed contents of a `.kicad_pcb` file.
/// The function scans for specific node types (segment, arc, via, footprint, gr_line, etc.)
/// and extracts geometry, coordinate, and electrical information.
///
/// # Coordinate Transform
/// KiCad uses a Y-down coordinate system (Y increases downward on screen).
/// This function negates all Y coordinates to convert to standard Y-up convention.
///
/// # Example
/// ```no_run
/// let content = std::fs::read_to_string("board.kicad_pcb")?;
/// let sexp_nodes = parse_sexp(&content)?;
/// let pcb_data = walk_kicad_tree(&sexp_nodes)?;
/// println!("Found {} traces", pcb_data.traces_fcu.len() + pcb_data.traces_bcu.len());
/// ```
pub fn walk_kicad_tree(nodes: &[SexpNode]) -> Result<PcbData> {
    let mut pcb = PcbData::default();
    let mut outline_segments = Vec::new();

    // Walk the top-level nodes
    for node in nodes {
        if let Some(list) = node.as_list() {
            if let Some(node_type) = list.first().and_then(|n| n.as_atom()) {
                match node_type {
                    // Straight trace segment on copper layer
                    "segment" => {
                        if let Ok(trace) = parse_segment(node) {
                            match trace.layer {
                                CopperLayer::FCu => pcb.traces_fcu.push(trace),
                                CopperLayer::BCu => pcb.traces_bcu.push(trace),
                            }
                        }
                    }

                    // Arc trace segment (less common)
                    "arc" => {
                        if let Ok(arc) = parse_arc(node) {
                            pcb.arc_traces.push(arc);
                        }
                    }

                    // Via hole connecting front and back layers
                    "via" => {
                        if let Ok(via) = parse_via(node) {
                            pcb.vias.push(via);
                        }
                    }

                    // Board outline or other graphic elements
                    "gr_line" | "gr_arc" | "gr_poly" => {
                        // Only process if on Edge.Cuts layer
                        if let Some(layer_node) = node.get_child("layer") {
                            if let Some(layer_name) = get_string_value(layer_node) {
                                if layer_name == "Edge.Cuts" {
                                    if node_type == "gr_line" {
                                        if let Some((start, end)) = parse_gr_line_points(node) {
                                            outline_segments.push((start, end));
                                        }
                                    } else if node_type == "gr_arc" {
                                        if let Some(_arc) = parse_gr_arc(node) {
                                            // For now, we'll handle arcs later
                                            // Could approximate as polyline here
                                        }
                                    } else if node_type == "gr_poly" {
                                        if let Ok(poly) = parse_gr_poly(node) {
                                            pcb.outline = Some(poly);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Component footprint (contains pads)
                    "footprint" => {
                        if let Ok(fp) = parse_footprint(node) {
                            pcb.pads.extend(fp.pads.iter().copied());
                            pcb.footprints.push(fp);
                        }
                    }

                    _ => {} // Ignore other node types
                }
            }
        }
    }

    // If we collected outline segments but no complete outline, try to chain them
    if pcb.outline.is_none() && !outline_segments.is_empty() {
        if let Ok(outline) = chain_outline_segments(outline_segments) {
            pcb.outline = Some(outline);
        }
    }

    Ok(pcb)
}

/// Parses a `(segment ...)` node representing a straight trace.
///
/// A segment looks like:
/// ```text
/// (segment (start 10.5 20.3) (end 50.2 40.1) (width 0.25) (layer "F.Cu") ...)
/// ```
fn parse_segment(node: &SexpNode) -> Result<Trace> {
    let start = node
        .get_child("start")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("segment missing (start X Y)"))?;

    let end = node
        .get_child("end")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("segment missing (end X Y)"))?;

    let width = node
        .get_child("width")
        .and_then(|n| n.nth(1))
        .and_then(|n| n.as_atom())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.25); // Default to 0.25mm if missing

    let layer = node
        .get_child("layer")
        .and_then(|n| get_string_value(n))
        .ok_or_else(|| anyhow!("segment missing (layer)"))?;

    let copper_layer = match layer.as_str() {
        "F.Cu" => CopperLayer::FCu,
        "B.Cu" => CopperLayer::BCu,
        _ => return Err(anyhow!("segment on non-copper layer: {}", layer)),
    };

    Ok(Trace {
        layer: copper_layer,
        start,
        end,
        width,
    })
}

/// Parses an `(arc ...)` node representing a curved trace.
///
/// An arc looks like:
/// ```text
/// (arc (start 10.5 20.3) (mid 20.0 15.0) (end 30.5 20.3) (layer "F.Cu") ...)
/// ```
///
/// The three-point arc format (start, midpoint, end) unambiguously defines which arc to use.
fn parse_arc(node: &SexpNode) -> Result<ArcTrace> {
    let start = node
        .get_child("start")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("arc missing (start X Y)"))?;

    let mid = node
        .get_child("mid")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("arc missing (mid X Y)"))?;

    let end = node
        .get_child("end")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("arc missing (end X Y)"))?;

    let layer = node
        .get_child("layer")
        .and_then(|n| get_string_value(n))
        .ok_or_else(|| anyhow!("arc missing (layer)"))?;

    let copper_layer = match layer.as_str() {
        "F.Cu" => CopperLayer::FCu,
        "B.Cu" => CopperLayer::BCu,
        _ => return Err(anyhow!("arc on non-copper layer: {}", layer)),
    };

    Ok(ArcTrace {
        layer: copper_layer,
        start,
        mid,
        end,
    })
}

/// Parses a `(via ...)` node.
///
/// A via looks like:
/// ```text
/// (via (at 25.0 30.0) (size 0.8) (drill 0.4) ...)
/// ```
///
/// Note: drill is actually the diameter (not radius).
fn parse_via(node: &SexpNode) -> Result<Via> {
    let center = node
        .get_child("at")
        .and_then(|n| get_xy_point(n))
        .ok_or_else(|| anyhow!("via missing (at X Y)"))?;

    // KiCad stores drill as a direct value (diameter)
    let drill = node
        .get_child("drill")
        .and_then(|n| n.nth(1))
        .and_then(|n| n.as_atom())
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| anyhow!("via missing (drill D)"))?;

    Ok(Via { center, drill })
}

/// Parses a `(gr_line ...)` node on Edge.Cuts layer.
///
/// A gr_line looks like:
/// ```text
/// (gr_line (start 0.0 0.0) (end 100.0 0.0) (layer "Edge.Cuts") ...)
/// ```
fn parse_gr_line_points(node: &SexpNode) -> Option<(Point2, Point2)> {
    let start = node.get_child("start").and_then(|n| get_xy_point(n))?;
    let end = node.get_child("end").and_then(|n| get_xy_point(n))?;
    Some((start, end))
}

/// Parses a `(gr_arc ...)` node on Edge.Cuts layer.
///
/// Similar to arcs on copper, but used for board outline.
fn parse_gr_arc(node: &SexpNode) -> Option<(Point2, Point2, Point2)> {
    let start = node.get_child("start").and_then(|n| get_xy_point(n))?;
    let mid = node.get_child("mid").and_then(|n| get_xy_point(n))?;
    let end = node.get_child("end").and_then(|n| get_xy_point(n))?;
    Some((start, mid, end))
}

/// Parses a `(gr_poly ...)` node on Edge.Cuts layer.
///
/// A gr_poly looks like:
/// ```text
/// (gr_poly (pts (xy 0.0 0.0) (xy 100.0 0.0) (xy 100.0 100.0) ...) (layer "Edge.Cuts") ...)
/// ```
fn parse_gr_poly(node: &SexpNode) -> Result<BoardOutline> {
    let pts_node = node
        .get_child("pts")
        .ok_or_else(|| anyhow!("gr_poly missing (pts)"))?;

    let mut vertices = Vec::new();

    if let Some(list) = pts_node.as_list() {
        for item in list {
            if let Some(xy_list) = item.as_list() {
                if let Some(xy_atom) = xy_list.first().and_then(|n| n.as_atom()) {
                    if xy_atom == "xy" {
                        if let Some(point) = get_xy_point(item) {
                            vertices.push(point);
                        }
                    }
                }
            }
        }
    }

    if vertices.is_empty() {
        return Err(anyhow!("gr_poly has no vertices"));
    }

    Ok(BoardOutline::new(vertices))
}

/// Parses a `(footprint ...)` node into a `Footprint` with reference, value, position, and pads.
fn parse_footprint(node: &SexpNode) -> Result<Footprint> {
    // Read footprint position in raw KiCad coords (Y-down, no negation yet)
    let at_node = node.get_child("at");
    let fp_x = at_node
        .and_then(|n| n.nth(1))
        .and_then(|n| n.as_atom())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fp_y = at_node
        .and_then(|n| n.nth(2))
        .and_then(|n| n.as_atom())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    // Rotation is at index 3 (optional, degrees, CCW in KiCad Y-down view)
    let fp_rot_deg = at_node
        .and_then(|n| n.nth(3))
        .and_then(|n| n.as_atom())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fp_rot = fp_rot_deg.to_radians();

    let position = Point2::new(fp_x, -fp_y);

    // Extract reference and value from (property "Reference" "R1") nodes
    let mut reference = String::new();
    let mut value = String::new();

    if let Some(list) = node.as_list() {
        for item in list {
            if let Some(item_list) = item.as_list() {
                if let Some(tag) = item_list.first().and_then(|n| n.as_atom()) {
                    if tag == "property" {
                        let prop_name = item_list.get(1).and_then(|n| n.as_atom()).unwrap_or("");
                        let prop_val = item_list.get(2).and_then(|n| n.as_atom()).unwrap_or("").to_string();
                        match prop_name {
                            "Reference" => reference = prop_val,
                            "Value" => value = prop_val,
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Walk through all pad elements
    let mut pads = Vec::new();
    if let Some(list) = node.as_list() {
        for item in list {
            if let Some(pad_list) = item.as_list() {
                if let Some(pad_type) = pad_list.first().and_then(|n| n.as_atom()) {
                    if pad_type == "pad" {
                        if let Some(at_node) = item.get_child("at") {
                            // Read pad position in raw KiCad coords (Y-down, no negation)
                            let pad_x = at_node.nth(1)
                                .and_then(|n| n.as_atom())
                                .and_then(|s| s.parse::<f64>().ok())
                                .unwrap_or(0.0);
                            let pad_y = at_node.nth(2)
                                .and_then(|n| n.as_atom())
                                .and_then(|s| s.parse::<f64>().ok())
                                .unwrap_or(0.0);

                            // Apply footprint rotation in KiCad Y-down space.
                            // KiCad uses CCW-positive in its Y-down view, which in
                            // Y-down coordinates uses the opposite sin sign vs standard
                            // Y-up math (flipping Y reverses rotation handedness).
                            // CCW in KiCad Y-down: x' = x*cos - y*(-sin) = x*cos + y*sin
                            //                      y' = x*(-sin) + y*cos = -x*sin + y*cos
                            let rot_x = pad_x * fp_rot.cos() + pad_y * fp_rot.sin();
                            let rot_y = -pad_x * fp_rot.sin() + pad_y * fp_rot.cos();
                            let absolute_pos = Point2::new(
                                fp_x + rot_x,
                                -(fp_y + rot_y),  // Y-up conversion
                            );

                            // Only include if pad has a drill
                            if let Some(drill_node) = item.get_child("drill") {
                                if let Some(drill_size) = drill_node
                                    .nth(1)
                                    .and_then(|n| n.as_atom())
                                    .and_then(|s| s.parse::<f64>().ok())
                                {
                                    pads.push(Pad {
                                        center: absolute_pos,
                                        drill: drill_size,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(Footprint { reference, value, position, pads })
}

/// Attempts to chain outline segments into a closed polygon.
///
/// KiCad's Edge.Cuts layer can have segments in any order. This function
/// sorts them by matching endpoints (within tolerance) to form a closed path.
///
/// Algorithm:
/// 1. Start with the first segment
/// 2. Find the next segment whose start point is close to the previous end point
/// 3. Repeat until all segments are used or closure fails
fn chain_outline_segments(mut segments: Vec<(Point2, Point2)>) -> Result<BoardOutline> {
    if segments.is_empty() {
        return Err(anyhow!("No outline segments to chain"));
    }

    let tolerance = 0.001; // millimeters
    let mut vertices = Vec::new();

    // Start with the first segment
    let (_current_start, mut current_end) = segments.remove(0);
    vertices.push(_current_start);
    vertices.push(current_end);

    // Keep chaining until all segments are used
    while !segments.is_empty() {
        let mut found = false;

        for i in 0..segments.len() {
            let (seg_start, seg_end) = segments[i];

            // Check if this segment continues from current_end
            if current_end.distance_to(seg_start) < tolerance {
                vertices.push(seg_end);
                current_end = seg_end;
                segments.remove(i);
                found = true;
                break;
            }

            // Check if this segment is reversed
            if current_end.distance_to(seg_end) < tolerance {
                vertices.push(seg_start);
                current_end = seg_start;
                segments.remove(i);
                found = true;
                break;
            }
        }

        if !found {
            return Err(anyhow!(
                "Could not chain outline segments: gap in perimeter at ({:.2}, {:.2})",
                current_end.x,
                current_end.y
            ));
        }
    }

    // Verify closure
    if vertices.last().map(|p| p.distance_to(vertices[0])) > Some(tolerance) {
        eprintln!("Warning: outline is not closed; first and last vertices are far apart");
    }

    Ok(BoardOutline::new(vertices))
}

/// Extracts an (X Y) coordinate pair from a node like `(start 10.5 20.3)`.
///
/// Returns the point with Y-coordinate negated to convert from KiCad's Y-down convention.
fn get_xy_point(node: &SexpNode) -> Option<Point2> {
    if let Some(list) = node.as_list() {
        if list.len() >= 3 {
            if let (Some(x_atom), Some(y_atom)) = (
                list[1].as_atom(),
                list[2].as_atom(),
            ) {
                if let (Ok(x), Ok(y)) = (x_atom.parse::<f64>(), y_atom.parse::<f64>()) {
                    // Negate Y to convert from KiCad's Y-down to standard Y-up
                    return Some(Point2::new(x, -y));
                }
            }
        }
    }
    None
}

/// Extracts a string value from a node like `(layer "F.Cu")`.
fn get_string_value(node: &SexpNode) -> Option<String> {
    if let Some(list) = node.as_list() {
        if let Some(value) = list.get(1) {
            return value.as_atom().map(|s| s.to_string());
        }
    }
    None
}
