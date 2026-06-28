// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Unified build guide generator.
//!
//! Produces a self-contained HTML file with three tabs:
//!   • Assembly Steps  — step-by-step board view with component/trace highlights
//!   • Continuity Test — net-by-net probe guide with pad overlay on board image
//!   • 3D Model        — interactive GLB viewer (requires kicad-cli)

use crate::config::{AssemblyStep, Config, Mode};
use crate::pcb::{ArcTrace, BoundingBox, PcbData, Point2};
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::Path;
use std::process::Command;

// ── Assembly step SVG dimensions ────────────────────────────────────────────
const SVG_W: f64 = 700.0;
const SVG_H: f64 = 500.0;
const PADDING: f64 = 20.0;

// ── Coordinate helpers ───────────────────────────────────────────────────────

struct ViewTransform {
    offset_x: f64,
    offset_y: f64,
    scale: f64,
}

impl ViewTransform {
    fn new(bbox: &BoundingBox) -> Self {
        let sx = (SVG_W - 2.0 * PADDING) / bbox.width().max(0.001);
        let sy = (SVG_H - 2.0 * PADDING) / bbox.height().max(0.001);
        let scale = sx.min(sy);
        ViewTransform {
            offset_x: PADDING - bbox.min_x * scale,
            offset_y: PADDING + bbox.max_y * scale,
            scale,
        }
    }

    fn px(&self, p: Point2) -> (f64, f64) {
        (
            self.offset_x + p.x * self.scale,
            self.offset_y - p.y * self.scale,
        )
    }
}

/// Convert Y-up PcbData coordinates to board-SVG coordinates (Y-down, board-relative).
fn to_board_svg(p: Point2, bbox: &BoundingBox) -> (f64, f64) {
    (p.x - bbox.min_x, bbox.max_y - p.y)
}

// ── External tool helpers ────────────────────────────────────────────────────

fn export_glb_base64(pcb_input: &Path) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!(
        "kicad2print_{}.glb",
        pcb_input.file_stem()?.to_str()?
    ));
    let status = Command::new("kicad-cli")
        .args(["pcb", "export", "glb", "--output", tmp.to_str()?, "--force", "--subst-models", pcb_input.to_str()?])
        .env("DISPLAY", "")
        .status()
        .ok()?;
    if !status.success() { return None; }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    Some(BASE64.encode(&bytes))
}

fn export_board_svg_b64(pcb_input: &Path) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!(
        "kicad2print_{}_board.svg",
        pcb_input.file_stem()?.to_str()?
    ));
    let status = Command::new("kicad-cli")
        .args([
            "pcb", "export", "svg",
            "--output", tmp.to_str()?,
            "--layers", "F.Cu,F.Silkscreen,Edge.Cuts",
            "--page-size-mode", "2",
            "--mode-single",
            pcb_input.to_str()?,
        ])
        .env("DISPLAY", "")
        .status()
        .ok()?;
    if !status.success() { return None; }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    Some(BASE64.encode(&bytes))
}

// ── Assembly step helpers ────────────────────────────────────────────────────

fn default_steps(pcb: &PcbData, config: &Config) -> Vec<AssemblyStep> {
    match config.mode {
        Mode::CopperWire => default_steps_copper_wire(pcb),
        Mode::Electrolysis => default_steps_electrolysis(pcb, config),
    }
}

fn default_steps_copper_wire(pcb: &PcbData) -> Vec<AssemblyStep> {
    let mut steps = Vec::new();
    if !pcb.footprints.is_empty() {
        steps.push(AssemblyStep {
            name: "Place components".to_string(),
            components: pcb.footprints.iter().map(|f| f.reference.clone()).collect(),
            wire_layer: None,
            instruction: "Insert through-hole components. Bend leads flush to the substrate surface.".to_string(),
        });
    }
    if !pcb.traces_fcu.is_empty() {
        steps.push(AssemblyStep {
            name: "Lay front-copper wires (F.Cu)".to_string(),
            components: vec![],
            wire_layer: Some("F.Cu".to_string()),
            instruction: "Lay 30 AWG wire into each highlighted groove on the TOP surface. Solder at each pad hole.".to_string(),
        });
    }
    if !pcb.traces_bcu.is_empty() {
        steps.push(AssemblyStep {
            name: "Lay back-copper wires (B.Cu)".to_string(),
            components: vec![],
            wire_layer: Some("B.Cu".to_string()),
            instruction: "Lay 30 AWG wire into each highlighted groove on the BOTTOM surface. Solder at each pad hole.".to_string(),
        });
    }
    if !pcb.vias.is_empty() {
        steps.push(AssemblyStep {
            name: "Connect vias".to_string(),
            components: vec![],
            wire_layer: None,
            instruction: "Insert copper eyelets into each via hole and solder top and bottom to bridge layers.".to_string(),
        });
    }
    steps
}

/// Copper plating statistics derived from the trace geometry and channel
/// dimensions. Used to populate the electroplating step of the build guide so
/// the user can calibrate their power supply and estimate plating time.
struct PlatingStats {
    /// Total length of all copper traces (F.Cu + B.Cu, straight + arc), mm.
    length_mm: f64,
    /// Top-projected copper area (length × channel width), cm².
    projected_cm2: f64,
    /// Wetted groove surface area (length × (width + 2·depth)), cm². This is the
    /// surface the seed coat covers and where plating current flows.
    wetted_cm2: f64,
    /// Recommended plating current at the assumed current density, mA.
    recommended_ma: f64,
    /// Rough time to fill the grooves to channel depth, minutes.
    fill_minutes: f64,
}

/// Assumed cathode current density for acid-copper plating, A/cm².
/// 20 mA/cm² is a conservative mid-range value for hobby CuSO₄ baths.
const PLATING_CURRENT_DENSITY: f64 = 0.020;

/// Length of a three-point arc trace (mm), falling back to the chord length for
/// degenerate (collinear) arcs.
fn arc_length(a: &ArcTrace) -> f64 {
    let (sx, sy) = (a.start.x, a.start.y);
    let (mx, my) = (a.mid.x, a.mid.y);
    let (ex, ey) = (a.end.x, a.end.y);
    let d = 2.0 * (sx * (my - ey) + mx * (ey - sy) + ex * (sy - my));
    if d.abs() < 1e-9 {
        return ((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt();
    }
    let s2 = sx * sx + sy * sy;
    let m2 = mx * mx + my * my;
    let e2 = ex * ex + ey * ey;
    let ux = (s2 * (my - ey) + m2 * (ey - sy) + e2 * (sy - my)) / d;
    let uy = (s2 * (ex - mx) + m2 * (sx - ex) + e2 * (mx - sx)) / d;
    let r = ((sx - ux).powi(2) + (sy - uy).powi(2)).sqrt();
    let norm = |x: f64| -> f64 {
        let t = std::f64::consts::TAU;
        ((x % t) + t) % t
    };
    let a1 = (sy - uy).atan2(sx - ux);
    let a2 = (ey - uy).atan2(ex - ux);
    let am = (my - uy).atan2(mx - ux);
    let s_to_e = norm(a2 - a1);
    let s_to_m = norm(am - a1);
    let sweep = if s_to_m <= s_to_e { s_to_e } else { std::f64::consts::TAU - s_to_e };
    r * sweep
}

/// Compute copper plating statistics from the PCB geometry and channel config.
fn plating_stats(pcb: &PcbData, channel_w: f64, channel_d: f64) -> PlatingStats {
    let mut length_mm = 0.0;
    for t in pcb.traces_fcu.iter().chain(pcb.traces_bcu.iter()) {
        length_mm += t.start.distance_to(t.end);
    }
    for a in &pcb.arc_traces {
        length_mm += arc_length(a);
    }

    // mm² → cm² (÷100).
    let projected_cm2 = length_mm * channel_w / 100.0;
    let wetted_cm2 = length_mm * (channel_w + 2.0 * channel_d) / 100.0;

    let current_a = wetted_cm2 * PLATING_CURRENT_DENSITY;
    let recommended_ma = current_a * 1000.0;

    // Faraday's law: charge to deposit enough copper to fill the grooves.
    //   Q = m·n·F / M, with m = ρ·V, V = length × width × depth.
    const M_CU: f64 = 63.55; // g/mol
    const N: f64 = 2.0; // electrons per Cu²⁺
    const FARADAY: f64 = 96485.0; // C/mol
    const RHO_CU: f64 = 8.96; // g/cm³
    const EFFICIENCY: f64 = 0.95; // current efficiency of acid-copper plating
    let fill_volume_cm3 = length_mm * channel_w * channel_d / 1000.0; // mm³ → cm³
    let mass_g = fill_volume_cm3 * RHO_CU;
    let charge_c = mass_g * N * FARADAY / M_CU / EFFICIENCY;
    let fill_minutes = if current_a > 0.0 { charge_c / current_a / 60.0 } else { 0.0 };

    PlatingStats { length_mm, projected_cm2, wetted_cm2, recommended_ma, fill_minutes }
}

/// Build the electroplating step instruction, embedding the computed copper area
/// and recommended power-supply settings.
fn electroplate_instruction(s: &PlatingStats, channel_d: f64) -> String {
    format!(
        "Connect the board as cathode in a copper sulfate (CuSO₄) bath. \
Copper to plate: ≈ {wetted:.1} cm² wetted groove surface ({proj:.1} cm² projected, {len:.0} mm total trace). \
Recommended current: ≈ {ma:.0} mA (at {cd:.0} mA/cm²), 1–2 V. \
Rough time to fill the {depth:.1} mm grooves: ≈ {mins:.0} min — scales inversely with current and is only an estimate \
(the wetted area shrinks as grooves fill), so finish by visual fill + continuity check rather than the clock. \
Rinse and dry when done.",
        wetted = s.wetted_cm2,
        proj = s.projected_cm2,
        len = s.length_mm,
        ma = s.recommended_ma,
        cd = PLATING_CURRENT_DENSITY * 1000.0,
        depth = channel_d,
        mins = s.fill_minutes,
    )
}

fn default_steps_electrolysis(pcb: &PcbData, config: &Config) -> Vec<AssemblyStep> {
    let mut steps = Vec::new();
    let stencil = config.generate_stencil;
    if !pcb.footprints.is_empty() {
        steps.push(AssemblyStep {
            name: "Place components".to_string(),
            components: pcb.footprints.iter().map(|f| f.reference.clone()).collect(),
            wire_layer: None,
            instruction: "Insert through-hole components into the substrate. Do not solder yet.".to_string(),
        });
    }
    if !pcb.traces_fcu.is_empty() {
        steps.push(AssemblyStep {
            name: "Prime front-copper grooves (F.Cu)".to_string(),
            components: vec![],
            wire_layer: Some("F.Cu".to_string()),
            instruction: if stencil {
                "Snap the TOP stencil (boardname_stencil_top.stl) onto the substrate, squeegee conductive seed paint across it so it fills the highlighted grooves and the bus rail/stubs, then lift it off. Let dry completely.".to_string()
            } else {
                "Apply conductive primer to all highlighted grooves on the TOP surface. Let dry completely.".to_string()
            },
        });
    }
    if !pcb.traces_bcu.is_empty() {
        steps.push(AssemblyStep {
            name: "Prime back-copper grooves (B.Cu)".to_string(),
            components: vec![],
            wire_layer: Some("B.Cu".to_string()),
            instruction: if stencil {
                "Snap the BOTTOM stencil (boardname_stencil_bottom.stl) onto the substrate, squeegee conductive seed paint across it so it fills the highlighted grooves and the bus rail/stubs, then lift it off. Let dry completely.".to_string()
            } else {
                "Apply conductive primer to all highlighted grooves on the BOTTOM surface. Let dry completely.".to_string()
            },
        });
    }
    if !pcb.traces_fcu.is_empty() || !pcb.traces_bcu.is_empty() {
        let stats = plating_stats(pcb, config.channel_width_mm, config.channel_depth_mm);
        steps.push(AssemblyStep {
            name: "Electroplate copper".to_string(),
            components: vec![],
            wire_layer: None,
            instruction: electroplate_instruction(&stats, config.channel_depth_mm),
        });
        if stencil {
            steps.push(AssemblyStep {
                name: "Grind off the plating bus".to_string(),
                components: vec![],
                wire_layer: None,
                instruction: "Sand or grind the raised bus rail and stubs flush with the surface to isolate the traces. The traces sit recessed in their grooves, so they stay untouched. Re-check continuity afterward — adjacent nets should now read open.".to_string(),
            });
        }
    }
    if !pcb.vias.is_empty() {
        steps.push(AssemblyStep {
            name: "Connect vias".to_string(),
            components: vec![],
            wire_layer: None,
            instruction: "Insert copper eyelets into each via hole and solder top and bottom to bridge layers.".to_string(),
        });
    }
    steps.push(AssemblyStep {
        name: "Test traces".to_string(),
        components: vec![],
        wire_layer: None,
        instruction: "Use a multimeter in continuity mode to verify each copper trace before soldering. See the Continuity Test tab.".to_string(),
    });
    steps
}

fn render_assembly_svg(pcb: &PcbData, step_idx: usize, steps: &[AssemblyStep]) -> String {
    let step = &steps[step_idx];

    let bbox = match &pcb.outline {
        Some(o) => o.bbox,
        None => {
            let mut b = BoundingBox::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
            for t in &pcb.traces_fcu { b.expand_to_include(t.start); b.expand_to_include(t.end); }
            for t in &pcb.traces_bcu { b.expand_to_include(t.start); b.expand_to_include(t.end); }
            for v in &pcb.vias { b.expand_to_include(v.center); }
            for p in &pcb.pads { b.expand_to_include(p.center); }
            b
        }
    };

    let vt = ViewTransform::new(&bbox);
    let mut svg = String::new();

    let _ = write!(svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {SVG_W} {SVG_H}\" style=\"width:100%;height:100%;display:block\">");

    // Board outline
    if let Some(outline) = &pcb.outline {
        let pts: Vec<String> = outline.vertices.iter().map(|&p| {
            let (x, y) = vt.px(p);
            format!("{x:.1},{y:.1}")
        }).collect();
        let _ = write!(svg, "<polygon points=\"{}\" fill=\"#111820\" stroke=\"#f0c040\" stroke-width=\"1.2\"/>", pts.join(" "));
    } else {
        let _ = write!(svg, "<rect width=\"{SVG_W}\" height=\"{SVG_H}\" fill=\"#111820\"/>");
    }

    let highlight_refs: std::collections::HashSet<&str> = step.components.iter().map(|s| s.as_str()).collect();
    let placed_refs: std::collections::HashSet<&str> = steps[..step_idx]
        .iter()
        .flat_map(|s| s.components.iter().map(|r| r.as_str()))
        .collect();

    for fp in &pcb.footprints {
        let is_highlight = highlight_refs.contains(fp.reference.as_str());
        let is_placed = placed_refs.contains(fp.reference.as_str());
        let (cx, cy) = vt.px(fp.position);
        let (pad_color, label_color, opacity) = if is_highlight {
            ("#00ff88", "#ffffff", "1.0")
        } else if is_placed {
            ("#336633", "#668866", "0.5")
        } else {
            ("#223322", "#334433", "0.35")
        };
        for pad in &fp.pads {
            let (px, py) = vt.px(pad.center);
            let r = (pad.drill * vt.scale / 2.0).max(3.0);
            let _ = write!(svg, "<circle cx=\"{px:.1}\" cy=\"{py:.1}\" r=\"{r:.1}\" fill=\"{pad_color}\" opacity=\"{opacity}\"/>");
        }
        let _ = write!(svg, "<text x=\"{cx:.1}\" y=\"{cy:.1}\" fill=\"{label_color}\" font-size=\"9\" font-family=\"monospace\" text-anchor=\"middle\" opacity=\"{opacity}\">{}</text>",
            html_escape(&fp.reference));
    }

    let fcu_done = steps[..step_idx].iter().any(|s| s.wire_layer.as_deref() == Some("F.Cu"));
    let bcu_done = steps[..step_idx].iter().any(|s| s.wire_layer.as_deref() == Some("B.Cu"));
    let show_fcu = step.wire_layer.as_deref() == Some("F.Cu");
    let show_bcu = step.wire_layer.as_deref() == Some("B.Cu");

    for trace in &pcb.traces_fcu {
        let (x1, y1) = vt.px(trace.start);
        let (x2, y2) = vt.px(trace.end);
        let (color, width, opacity) = if show_fcu { ("#e94560", "2.5", "1.0") } else if fcu_done { ("#7a2233", "1.5", "0.5") } else { ("#3a1118", "1.0", "0.3") };
        let _ = write!(svg, "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" stroke=\"{color}\" stroke-width=\"{width}\" opacity=\"{opacity}\" stroke-linecap=\"round\"/>");
    }
    for trace in &pcb.traces_bcu {
        let (x1, y1) = vt.px(trace.start);
        let (x2, y2) = vt.px(trace.end);
        let (color, width, opacity) = if show_bcu { ("#4488ff", "2.5", "1.0") } else if bcu_done { ("#1a3366", "1.5", "0.5") } else { ("#0a1133", "1.0", "0.3") };
        let _ = write!(svg, "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" stroke=\"{color}\" stroke-width=\"{width}\" opacity=\"{opacity}\" stroke-linecap=\"round\"/>");
    }

    let vias_done = steps[..step_idx].iter().any(|s| s.wire_layer.is_none() && s.components.is_empty() && s.name.to_lowercase().contains("via"));
    let vias_active = step.name.to_lowercase().contains("via") && step.components.is_empty() && step.wire_layer.is_none();
    for via in &pcb.vias {
        let (cx, cy) = vt.px(via.center);
        let r = (via.drill * vt.scale / 2.0).max(3.0);
        let (color, opacity) = if vias_active { ("#f5a623", "1.0") } else if vias_done { ("#7a5100", "0.6") } else { ("#333300", "0.3") };
        let _ = write!(svg, "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{r:.1}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.5\" opacity=\"{opacity}\"/>");
    }

    svg.push_str("</svg>");
    svg
}

fn build_parts_table(pcb: &PcbData, step: &AssemblyStep) -> String {
    if step.components.is_empty() { return String::new(); }
    let mut rows = String::new();
    for refdes in &step.components {
        let fp = pcb.footprints.iter().find(|f| &f.reference == refdes);
        let value = fp.map(|f| f.value.as_str()).unwrap_or("\u{2014}");
        let _ = write!(rows,
            "<tr><td style=\"padding:4px 10px;color:#00ff88;font-family:monospace\">{}</td><td style=\"padding:4px 10px;color:#cccccc\">{}</td></tr>",
            html_escape(refdes), html_escape(value));
    }
    format!("<table style=\"border-collapse:collapse;font-size:13px;margin-top:10px\"><tr><th style=\"padding:4px 10px;color:#8899aa;text-align:left\">Ref</th><th style=\"padding:4px 10px;color:#8899aa;text-align:left\">Value</th></tr>{rows}</table>")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// ── Continuity test helpers ──────────────────────────────────────────────────

struct PadEntry {
    id: String,
    reference: String,
    number: String,
    net: String,
    svg_x: f64,
    svg_y: f64,
    drill_r: f64,
}

struct ContinuityData {
    pads_js: String,
    steps_js: String,
    total_nets: usize,
    viewbox: String,
    bg_rect: String,
    board_outline_svg: String,
    board_img: String,
}

fn collect_continuity(pcb: &PcbData, board_svg_b64: &Option<String>) -> Option<ContinuityData> {
    let bbox = match &pcb.outline {
        Some(o) => o.bbox,
        None => {
            let mut b = BoundingBox::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
            for fp in &pcb.footprints {
                for pad in &fp.pads { b.expand_to_include(pad.center); }
            }
            if b.min_x.is_infinite() { return None; }
            b
        }
    };

    let board_w = bbox.width();
    let board_h = bbox.height();

    // Count pads per net to distinguish real multi-pad "unconnected-" nets
    // (e.g. an unnamed power rail) from truly isolated single-pad ones.
    let mut net_pad_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for fp in &pcb.footprints {
        for pad in &fp.pads {
            if let Some(ref net) = pad.net_name {
                if !net.is_empty() {
                    *net_pad_count.entry(net.as_str()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut all_pads: Vec<PadEntry> = Vec::new();
    for fp in &pcb.footprints {
        for pad in &fp.pads {
            let Some(ref net) = pad.net_name else { continue };
            if net.is_empty() { continue }
            if net.starts_with("unconnected") {
                // Skip truly isolated pads; keep unconnected nets shared by 2+ pads
                if net_pad_count.get(net.as_str()).copied().unwrap_or(0) < 2 { continue }
            }
            let (sx, sy) = to_board_svg(pad.center, &bbox);
            all_pads.push(PadEntry {
                id: format!("{}-{}", fp.reference, pad.number),
                reference: fp.reference.clone(),
                number: pad.number.clone(),
                net: net.clone(),
                svg_x: sx,
                svg_y: sy,
                drill_r: (pad.drill / 2.0).max(0.8),
            });
        }
    }
    if all_pads.is_empty() { return None; }

    let pad_margin = 5.0_f64;
    let vb_min_x = all_pads.iter().map(|p| p.svg_x).fold(0.0_f64, f64::min) - pad_margin;
    let vb_min_y = all_pads.iter().map(|p| p.svg_y).fold(0.0_f64, f64::min) - pad_margin;
    let vb_max_x = all_pads.iter().map(|p| p.svg_x).fold(board_w, f64::max) + pad_margin;
    let vb_max_y = all_pads.iter().map(|p| p.svg_y).fold(board_h, f64::max) + pad_margin;
    let vb_w = vb_max_x - vb_min_x;
    let vb_h = vb_max_y - vb_min_y;

    let mut by_net: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, pad) in all_pads.iter().enumerate() {
        by_net.entry(pad.net.clone()).or_default().push(i);
    }

    let mut pads_js = String::new();
    for (i, p) in all_pads.iter().enumerate() {
        if i > 0 { pads_js.push(','); }
        let _ = write!(pads_js, "{{id:{id:?},ref:{ref_:?},num:{num:?},net:{net:?},x:{x:.3},y:{y:.3},r:{r:.3}}}",
            id = p.id, ref_ = p.reference, num = p.number, net = p.net,
            x = p.svg_x, y = p.svg_y, r = p.drill_r);
    }

    let mut steps_js = String::new();
    for (i, (net, indices)) in by_net.iter().enumerate() {
        if i > 0 { steps_js.push(','); }
        let idx_str: Vec<String> = indices.iter().map(|n| n.to_string()).collect();
        let _ = write!(steps_js, "{{net:{net:?},pads:[{pads}]}}", net = net, pads = idx_str.join(","));
    }

    let total_nets = by_net.len();
    let viewbox = format!("{vb_min_x:.2} {vb_min_y:.2} {vb_w:.2} {vb_h:.2}");

    let bg_rect = format!(
        "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"#111820\"/>",
        vb_min_x, vb_min_y, vb_w, vb_h
    );
    let board_outline_svg = format!(
        "<rect x=\"0\" y=\"0\" width=\"{board_w:.4}\" height=\"{board_h:.4}\" fill=\"none\" stroke=\"#f0c040\" stroke-width=\"0.25\" stroke-dasharray=\"1.5 0.8\"/>"
    );
    let board_img = match board_svg_b64 {
        Some(b64) => format!(
            "<image href=\"data:image/svg+xml;base64,{b64}\" x=\"0\" y=\"0\" width=\"{board_w:.4}\" height=\"{board_h:.4}\"/>"
        ),
        None => String::new(),
    };

    Some(ContinuityData {
        pads_js, steps_js, total_nets, viewbox, bg_rect, board_outline_svg, board_img,
    })
}

// ── Main write function ──────────────────────────────────────────────────────

pub fn write(pcb: &PcbData, pcb_input: &Path, config: &Config, stem: &str, path: &Path) -> Result<()> {
    let steps: Vec<AssemblyStep> = if config.assembly_steps.is_empty() {
        default_steps(pcb, config)
    } else {
        config.assembly_steps.to_vec()
    };
    if steps.is_empty() { return Ok(()); }

    // Pre-render assembly step SVGs and build JS arrays
    let mut a_svgs: Vec<String> = Vec::new();
    let mut a_titles: Vec<String> = Vec::new();
    let mut a_instructions: Vec<String> = Vec::new();
    let mut a_parts: Vec<String> = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        a_svgs.push(render_assembly_svg(pcb, i, &steps));
        a_titles.push(html_escape(&step.name));
        a_instructions.push(html_escape(&step.instruction));
        a_parts.push(build_parts_table(pcb, step));
    }

    // Export external assets (best-effort)
    let glb_b64 = export_glb_base64(pcb_input);
    let board_svg_b64 = export_board_svg_b64(pcb_input);

    // Collect continuity data
    let ct = collect_continuity(pcb, &board_svg_b64);
    let has_ct = ct.is_some();

    let stem_esc = html_escape(stem);
    let a_total = steps.len();
    let glb_b64_literal = match &glb_b64 {
        Some(b64) => format!("\"{b64}\""),
        None => "null".to_string(),
    };

    // ── Build HTML ──────────────────────────────────────────────────────────
    let mut html = String::new();

    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"UTF-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    let _ = write!(html, "<title>Build Guide \u{2014} {stem_esc}</title>\n");
    html.push_str("<style>\n");
    html.push_str(CSS);
    html.push_str("\n</style>\n</head>\n<body>\n");

    // Header
    let _ = write!(html,
        "<header>\n  <h1>Build Guide \u{2014} {stem_esc}</h1>\n  <p>Assembly steps, continuity test, and 3D model</p>\n</header>\n");

    // Tab bar
    html.push_str("<div class=\"tab-bar\">\n");
    let _ = write!(html, "  <button class=\"tab-btn active\" onclick=\"switchTab('assembly')\">Assembly Steps</button>\n");
    if has_ct {
        html.push_str("  <button class=\"tab-btn\" onclick=\"switchTab('continuity')\">Continuity Test</button>\n");
    }
    html.push_str("  <button class=\"tab-btn\" onclick=\"switchTab('3d')\">3D Model</button>\n");
    html.push_str("</div>\n");

    // ── Assembly tab ────────────────────────────────────────────────────────
    html.push_str("<div id=\"tab-assembly\" class=\"layout\">\n");
    // Sidebar
    html.push_str("  <div class=\"steps-panel\">\n    <h2>Assembly Steps</h2>\n    <div id=\"a-steps-list\"></div>\n  </div>\n");
    // Board panel
    html.push_str("  <div class=\"board-panel\">\n");
    html.push_str("    <div class=\"board-title\">Board view \u{2014} highlighted elements for this step</div>\n");
    html.push_str("    <div id=\"a-board\"></div>\n");
    html.push_str("    <div class=\"legend-row\">\n");
    html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#00ff88\"></span>This step</span>\n");
    html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#336633\"></span>Placed</span>\n");
    html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#e94560;height:3px;border-radius:0\"></span>F.Cu wire</span>\n");
    html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#4488ff;height:3px;border-radius:0\"></span>B.Cu wire</span>\n");
    html.push_str("    </div>\n");
    html.push_str("    <div class=\"progress-bar-wrap\"><div class=\"progress-bar\" id=\"a-progress-bar\" style=\"width:0%\"></div></div>\n");
    html.push_str("  </div>\n");
    // Detail panel
    html.push_str("  <div class=\"detail-panel\" id=\"a-detail\">\n");
    html.push_str("    <div id=\"a-step-content\"><p class=\"placeholder\">Select a step from the left panel to begin.</p></div>\n");
    html.push_str("    <div id=\"a-completion\" style=\"display:none\" class=\"completion\">\n");
    html.push_str("      <div class=\"check-icon\">&#10003;</div>\n");
    let _ = write!(html, "      <h2>Assembly Complete!</h2>\n      <p>All {a_total} steps done. Ready for continuity testing.</p>\n");
    html.push_str("    </div>\n  </div>\n");
    html.push_str("</div>\n");

    // ── Continuity tab ──────────────────────────────────────────────────────
    if let Some(ct_data) = &ct {
        html.push_str("<div id=\"tab-continuity\" class=\"layout\" style=\"display:none\">\n");
        // Sidebar
        let _ = write!(html,
            "  <div class=\"steps-panel\">\n    <h2>Nets ({} total)</h2>\n    <div id=\"c-steps-list\"></div>\n  </div>\n",
            ct_data.total_nets);
        // Board panel
        html.push_str("  <div class=\"board-panel\">\n");
        html.push_str("    <div class=\"board-title\">Board diagram \u{2014} pads highlighted per net</div>\n");
        let _ = write!(html,
            "    <svg id=\"c-board-svg\" viewBox=\"{}\" xmlns=\"http://www.w3.org/2000/svg\" style=\"display:block\">\n",
            ct_data.viewbox);
        html.push_str("      ");
        html.push_str(&ct_data.bg_rect);
        html.push('\n');
        if !ct_data.board_img.is_empty() {
            html.push_str("      ");
            html.push_str(&ct_data.board_img);
            html.push('\n');
        }
        html.push_str("      ");
        html.push_str(&ct_data.board_outline_svg);
        html.push('\n');
        html.push_str("      <g id=\"c-pads-layer\"></g>\n");
        html.push_str("      <g id=\"c-labels-layer\"></g>\n");
        html.push_str("    </svg>\n");
        html.push_str("    <div class=\"legend-row\">\n");
        html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#445566\"></span>Untested</span>\n");
        html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#e94560\"></span>RED anchor</span>\n");
        html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#f5a623\"></span>BLACK sweep</span>\n");
        html.push_str("      <span class=\"leg\"><span class=\"leg-dot\" style=\"background:#4caf50\"></span>Passed</span>\n");
        html.push_str("    </div>\n");
        html.push_str("    <div class=\"progress-bar-wrap\"><div class=\"progress-bar\" id=\"c-progress-bar\" style=\"width:0%\"></div></div>\n");
        html.push_str("  </div>\n");
        // Detail panel
        html.push_str("  <div class=\"detail-panel\" id=\"c-detail\">\n");
        html.push_str("    <div id=\"c-step-content\"><p class=\"placeholder\">Select a net from the left panel to begin.</p></div>\n");
        html.push_str("    <div id=\"c-completion\" style=\"display:none\" class=\"completion\">\n");
        html.push_str("      <div class=\"check-icon\">&#10003;</div>\n");
        let _ = write!(html, "      <h2>All Tests Passed!</h2>\n      <p>All {} nets verified. Board wiring is correct.</p>\n",
            ct_data.total_nets);
        html.push_str("    </div>\n  </div>\n");
        html.push_str("</div>\n");
    }

    // ── 3D tab ──────────────────────────────────────────────────────────────
    html.push_str("<div id=\"tab-3d\" style=\"display:none;height:calc(100vh - 120px);flex-direction:column;padding:0.75rem 1rem;max-width:1400px;margin:0 auto;box-sizing:border-box\">\n");
    html.push_str("  <canvas id=\"canvas3d\" style=\"flex:1;min-height:0;width:100%;border-radius:8px;background:#1a1a2e;display:block\"></canvas>\n");
    html.push_str("  <p style=\"font-size:12px;color:#8899aa;text-align:center;margin-top:8px\">Drag: Rotate &nbsp;|&nbsp; Scroll: Zoom &nbsp;|&nbsp; Right-drag: Pan</p>\n");
    html.push_str("  <p id=\"no-3d\" style=\"display:none;color:#8899aa;text-align:center;padding:2rem\">3D model unavailable \u{2014} kicad-cli GLB export failed or kicad-cli is not installed.</p>\n");
    html.push_str("</div>\n");

    // ── Scripts ─────────────────────────────────────────────────────────────
    html.push_str("<script src=\"https://cdnjs.cloudflare.com/ajax/libs/three.js/r128/three.min.js\"></script>\n");
    html.push_str("<script src=\"https://cdn.jsdelivr.net/npm/three@0.128.0/examples/js/loaders/GLTFLoader.js\"></script>\n");
    html.push_str("<script>\n");

    // Assembly JS data
    let svgs_literal: Vec<String> = a_svgs.iter().map(|s| {
        format!("`{}`", s.replace('`', "\\`").replace("${", "\\${"))
    }).collect();
    let titles_literal: Vec<String> = a_titles.iter().map(|s| {
        format!("`{}`", s.replace('`', "\\`"))
    }).collect();
    let instrs_literal: Vec<String> = a_instructions.iter().map(|s| {
        format!("`{}`", s.replace('`', "\\`"))
    }).collect();
    let parts_literal: Vec<String> = a_parts.iter().map(|s| {
        format!("`{}`", s.replace('`', "\\`").replace("${", "\\${"))
    }).collect();

    let _ = write!(html, "const A_SVGS=[{}];\n", svgs_literal.join(","));
    let _ = write!(html, "const A_TITLES=[{}];\n", titles_literal.join(","));
    let _ = write!(html, "const A_INSTRS=[{}];\n", instrs_literal.join(","));
    let _ = write!(html, "const A_PARTS=[{}];\n", parts_literal.join(","));
    let _ = write!(html, "const A_TOTAL={a_total};\n");

    // Continuity JS data
    if let Some(ct_data) = &ct {
        let _ = write!(html, "const C_PADS=[{}];\n", ct_data.pads_js);
        let _ = write!(html, "const C_STEPS=[{}];\n", ct_data.steps_js);
        let _ = write!(html, "const C_TOTAL={};\n", ct_data.total_nets);
    } else {
        html.push_str("const C_PADS=[];\nconst C_STEPS=[];\nconst C_TOTAL=0;\n");
    }

    let _ = write!(html, "const GLB_B64={glb_b64_literal};\n");

    html.push_str(ASSEMBLY_JS);
    html.push_str(CONTINUITY_JS);
    html.push_str(THREED_JS);
    html.push_str(TAB_JS);

    html.push_str("</script>\n</body>\n</html>");

    let mut file = std::fs::File::create(path)?;
    file.write_all(html.as_bytes())?;
    Ok(())
}

// ── Static CSS ───────────────────────────────────────────────────────────────

const CSS: &str = r#"
:root {
  --bg:#1a1a2e; --panel:#16213e; --card:#0f3460; --accent:#e94560;
  --gold:#f5a623; --green:#4caf50; --text:#eaeaea; --muted:#8899aa;
  --border:#2a3a5a; --pad-anchor:#e94560; --pad-sweep:#f5a623; --pad-done:#4caf50;
}
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:'Segoe UI',system-ui,sans-serif;background:var(--bg);color:var(--text);min-height:100vh}
header{background:linear-gradient(135deg,#0f3460 0%,#16213e 100%);border-bottom:2px solid var(--accent);padding:1rem 2rem;text-align:center}
header h1{font-size:1.4rem}
header p{color:var(--muted);font-size:0.85rem;margin-top:0.25rem}

/* Tab bar */
.tab-bar{display:flex;gap:0;border-bottom:1px solid var(--border);background:var(--panel)}
.tab-btn{background:transparent;border:none;border-bottom:2px solid transparent;color:var(--muted);padding:0.7rem 1.5rem;font-size:0.88rem;cursor:pointer;transition:color 0.15s,border-color 0.15s;margin-bottom:-1px}
.tab-btn:hover{color:var(--text)}
.tab-btn.active{color:var(--accent);border-bottom-color:var(--accent);font-weight:600}

/* Main layout grid — sidebar + board/detail column */
.layout{display:grid;grid-template-columns:300px 1fr;grid-template-rows:1fr auto;gap:0;height:calc(100vh - 120px)}
.steps-panel{background:var(--panel);border-right:1px solid var(--border);overflow-y:auto;grid-row:1/3}
.steps-panel h2{font-size:0.75rem;text-transform:uppercase;letter-spacing:0.1em;color:var(--muted);padding:0.85rem 1rem 0.5rem;border-bottom:1px solid var(--border);position:sticky;top:0;background:var(--panel);z-index:1}
.step-item{padding:0.6rem 0.9rem;border-bottom:1px solid var(--border);cursor:pointer;transition:background 0.15s;display:flex;gap:0.6rem;align-items:flex-start}
.step-item:hover{background:rgba(233,69,96,0.08)}
.step-item.active{background:rgba(233,69,96,0.15);border-left:3px solid var(--accent)}
.step-item.done{opacity:0.55}
.step-num{min-width:26px;height:26px;border-radius:50%;background:var(--card);display:flex;align-items:center;justify-content:center;font-size:0.75rem;font-weight:bold;border:2px solid var(--border);flex-shrink:0}
.step-item.active .step-num{border-color:var(--accent);color:var(--accent)}
.step-item.done .step-num{background:var(--green);border-color:var(--green);color:#fff}
.step-meta{flex:1;min-width:0}
.step-name{font-size:0.85rem;font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.step-brief{font-size:0.74rem;color:var(--muted);margin-top:0.15rem}

/* Board panel (top of right column) — flex column, fills its 1fr grid row */
.board-panel{padding:0.6rem 1rem 0.5rem;background:var(--bg);display:flex;flex-direction:column;align-items:stretch;overflow:hidden;min-height:0}
.board-title{font-size:0.68rem;text-transform:uppercase;letter-spacing:0.1em;color:var(--muted);margin-bottom:0.35rem;flex-shrink:0}
#a-board{flex:1;min-height:0;width:100%;overflow:hidden;display:flex;align-items:center}
#a-board > svg{width:100%;height:100%}
#c-board-svg{flex:1;min-height:0;width:100%}

/* Active pad animations */
@keyframes anchorGlow {
  0%,100%{opacity:1;filter:drop-shadow(0 0 1px #e94560)}
  50%{opacity:0.5;filter:drop-shadow(0 0 4px #ff3366) drop-shadow(0 0 8px #e94560bb)}
}
@keyframes sweepBlink {
  0%,100%{opacity:1}
  50%{opacity:0.2}
}
.pad-anchor{animation:anchorGlow 0.75s ease-in-out infinite}
.pad-sweep{animation:sweepBlink 1.0s ease-in-out infinite}

.legend-row{display:flex;gap:1rem;margin-top:0.4rem;flex-wrap:wrap;flex-shrink:0}
.leg{display:flex;align-items:center;gap:0.3rem;font-size:0.75rem;color:var(--muted)}
.leg-dot{width:12px;height:12px;border-radius:50%;display:inline-block;flex-shrink:0}
.progress-bar-wrap{height:3px;background:var(--border);width:100%;border-radius:2px;margin-top:0.6rem}
.progress-bar{height:3px;background:var(--green);border-radius:2px;transition:width 0.3s}

/* Detail panel (bottom of right column) */
.detail-panel{background:var(--panel);border-top:1px solid var(--border);padding:1rem 1.2rem;overflow-y:auto}
.detail-panel h2{font-size:1rem;color:var(--accent);margin-bottom:0.2rem;display:flex;align-items:center;gap:0.5rem}
.net-badge{font-size:0.72rem;background:var(--card);border:1px solid var(--border);border-radius:4px;padding:0.1rem 0.4rem;color:var(--gold);font-family:monospace}
.placeholder{color:var(--muted);text-align:center;margin-top:1.5rem;font-size:0.9rem}

/* Probe layout */
.probe-section{margin-top:0.6rem}
.probe-label{font-size:0.72rem;text-transform:uppercase;letter-spacing:0.08em;font-weight:bold;margin-bottom:0.3rem}
.probe-anchor{color:var(--pad-anchor)}
.probe-sweep{color:var(--pad-sweep)}
.probe-point{display:flex;align-items:center;gap:0.4rem;padding:0.3rem 0.45rem;border-radius:4px;margin-bottom:0.2rem;font-size:0.82rem;background:rgba(255,255,255,0.04)}
.probe-dot{width:9px;height:9px;border-radius:50%;flex-shrink:0}
.probe-ref{font-family:monospace;font-weight:bold;min-width:65px}
.probe-desc{color:var(--muted);font-size:0.77rem}

/* Navigation */
.nav-buttons{display:flex;gap:0.6rem;margin-top:0.9rem}
.btn{padding:0.4rem 1rem;border-radius:5px;border:none;cursor:pointer;font-size:0.82rem;font-weight:600;transition:opacity 0.15s}
.btn:hover{opacity:0.85}
.btn:disabled{opacity:0.3;cursor:default}
.btn-done{background:var(--green);color:#fff}
.btn-next{background:var(--accent);color:#fff}
.btn-prev{background:var(--card);color:var(--text);border:1px solid var(--border)}

/* Completion screen */
.completion{text-align:center;padding:2rem 1rem}
.completion h2{font-size:1.6rem;color:var(--green);margin-bottom:0.6rem}
.completion p{color:var(--muted)}
.check-icon{font-size:3rem}
"#;

// ── Static JS ────────────────────────────────────────────────────────────────

const ASSEMBLY_JS: &str = r#"
// ── Assembly tab ─────────────────────────────────────────────────────────────
let aCur = 0;
const aDone = new Set();

function aRenderSidebar() {
  document.getElementById('a-steps-list').innerHTML = A_TITLES.map((t,i) => {
    const cls = i===aCur ? 'step-item active' : aDone.has(i) ? 'step-item done' : 'step-item';
    const num = aDone.has(i) ? '✓' : i+1;
    return `<div class="${cls}" onclick="aGoStep(${i})"><div class="step-num">${num}</div><div class="step-meta"><div class="step-name">${t}</div></div></div>`;
  }).join('');
}

function aRenderBoard() {
  document.getElementById('a-board').innerHTML = A_SVGS[aCur];
}

function aRenderDetail() {
  const content = document.getElementById('a-step-content');
  const prevDis = aCur===0 ? ' disabled' : '';
  const nextDis = aCur===A_TOTAL-1 ? ' disabled' : '';
  content.innerHTML =
    `<h2>Step ${aCur+1}: ${A_TITLES[aCur]}</h2>`
    + `<p style="font-size:0.82rem;color:var(--muted);margin:0.5rem 0">${A_INSTRS[aCur]}</p>`
    + A_PARTS[aCur]
    + `<div class="nav-buttons">`
    + `<button class="btn btn-prev"${prevDis} onclick="aGoStep(aCur-1)">← Prev</button>`
    + `<button class="btn btn-done" onclick="aMarkDone()">✓ Done</button>`
    + `<button class="btn btn-next"${nextDis} onclick="aGoStep(aCur+1)">Next →</button>`
    + `</div>`;
}

function aUpdateProgress() {
  const pct = A_TOTAL > 0 ? (aDone.size / A_TOTAL * 100) : 0;
  document.getElementById('a-progress-bar').style.width = pct + '%';
  if (aDone.size === A_TOTAL) {
    document.getElementById('a-step-content').style.display = 'none';
    document.getElementById('a-completion').style.display = 'block';
  }
}

function aGoStep(i) {
  if (i < 0 || i >= A_TOTAL) return;
  aCur = i;
  aRenderSidebar(); aRenderBoard(); aRenderDetail(); aUpdateProgress();
  const items = document.querySelectorAll('#a-steps-list .step-item');
  if (items[i]) items[i].scrollIntoView({block:'nearest'});
}

function aMarkDone() {
  aDone.add(aCur);
  if (aCur < A_TOTAL - 1) aGoStep(aCur + 1);
  else { aRenderSidebar(); aUpdateProgress(); }
}
"#;

const CONTINUITY_JS: &str = r#"
// ── Continuity tab ───────────────────────────────────────────────────────────
let cCur = -1;
const cDone = new Set();
const cPadIdx = {};
C_PADS.forEach((p,i) => cPadIdx[p.id] = i);

function cSvgPad(pad, role) {
  const fill = role==='anchor' ? '#e94560' : role==='sweep' ? '#f5a623' : role==='done' ? '#4caf50' : '#445566';
  const stroke = role==='anchor' ? 'white' : 'none';
  const sw = role==='anchor' ? '0.2' : '0';
  const r = role==='anchor' || role==='sweep' ? (pad.r * 1.35).toFixed(3) : pad.r.toFixed(3);
  const cls = role==='anchor' ? ' class="pad-anchor"' : role==='sweep' ? ' class="pad-sweep"' : '';
  return `<circle${cls} id="cpad-${pad.id}" cx="${pad.x.toFixed(3)}" cy="${pad.y.toFixed(3)}" r="${r}" fill="${fill}" stroke="${stroke}" stroke-width="${sw}" style="cursor:pointer" onclick="cPadClick('${pad.id}')"/>`;
}

function cSvgLabel(pad) {
  return `<text x="${(pad.x+pad.r*1.3).toFixed(3)}" y="${(pad.y+pad.r*0.5).toFixed(3)}" font-size="1.2" fill="white" font-family="monospace">${pad.ref}-${pad.num}</text>`;
}

function cRenderBoard() {
  const pLayer = document.getElementById('c-pads-layer');
  const lLayer = document.getElementById('c-labels-layer');
  if (!pLayer) return;
  let pSvg = '', lSvg = '';
  if (cCur < 0) {
    C_PADS.forEach(p => { pSvg += cSvgPad(p, 'default'); });
  } else {
    const step = C_STEPS[cCur];
    const stepIds = new Set(step.pads.map(i => C_PADS[i].id));
    C_PADS.forEach(p => { if (!stepIds.has(p.id)) pSvg += cSvgPad(p, 'default'); });
    step.pads.forEach((pi, si) => {
      const p = C_PADS[pi];
      pSvg += cSvgPad(p, si===0 ? 'anchor' : 'sweep');
      lSvg += cSvgLabel(p);
    });
  }
  pLayer.innerHTML = pSvg;
  lLayer.innerHTML = lSvg;
}

function cRenderSidebar() {
  const list = document.getElementById('c-steps-list');
  if (!list) return;
  list.innerHTML = C_STEPS.map((s,i) => {
    const cls = i===cCur ? 'step-item active' : cDone.has(i) ? 'step-item done' : 'step-item';
    const num = cDone.has(i) ? '✓' : i+1;
    const brief = s.pads.length + ' pad' + (s.pads.length!==1?'s':'');
    const isUnnamed = s.net.startsWith('unconnected-(');
    const displayNet = isUnnamed ? '⚠ ' + s.net.replace(/^unconnected-\((.+)\)$/, '$1') : s.net;
    const netStyle = isUnnamed ? 'font-family:monospace;color:var(--gold);font-size:0.8rem' : 'font-family:monospace';
    return `<div class="${cls}" onclick="cGoStep(${i})"><div class="step-num">${num}</div><div class="step-meta"><div class="step-name" style="${netStyle}">${displayNet}</div><div class="step-brief">${brief}</div></div></div>`;
  }).join('');
}

function cRenderDetail() {
  const content = document.getElementById('c-step-content');
  if (!content) return;
  if (cCur < 0) {
    content.innerHTML = '<p class="placeholder">Select a net from the left panel to begin.</p>';
    return;
  }
  const step = C_STEPS[cCur];
  const anchor = C_PADS[step.pads[0]];
  const sweeps = step.pads.slice(1).map(i => C_PADS[i]);
  const anchorHtml = `<div class="probe-point"><div class="probe-dot" style="background:#e94560"></div><span class="probe-ref">${anchor.ref}-${anchor.num}</span><span class="probe-desc">net ${anchor.net}</span></div>`;
  const sweepHtml = sweeps.map(p => `<div class="probe-point"><div class="probe-dot" style="background:#f5a623"></div><span class="probe-ref">${p.ref}-${p.num}</span><span class="probe-desc">should beep</span></div>`).join('');
  const prevDis = cCur===0 ? ' disabled' : '';
  const nextDis = cCur===C_TOTAL-1 ? ' disabled' : '';
  const isUnnamedNet = step.net.startsWith('unconnected-(');
  const displayNetName = isUnnamedNet ? step.net.replace(/^unconnected-\((.+)\)$/, '$1') + ' (unnamed)' : step.net;
  content.innerHTML =
    `<h2>Net: <span class="net-badge">${displayNetName}</span></h2>`
    + `<p style="font-size:0.82rem;color:var(--muted);margin-bottom:0.4rem">Step ${cCur+1} of ${C_TOTAL}</p>`
    + `<div class="probe-section"><div class="probe-label probe-anchor">RED probe — hold here</div>${anchorHtml}</div>`
    + `<div class="probe-section" style="margin-top:0.5rem"><div class="probe-label probe-sweep">BLACK probe — touch each</div>${sweepHtml}</div>`
    + `<div class="nav-buttons">`
    + `<button class="btn btn-prev"${prevDis} onclick="cGoStep(cCur-1)">← Prev</button>`
    + `<button class="btn btn-done" onclick="cMarkDone()">✓ Done</button>`
    + `<button class="btn btn-next"${nextDis} onclick="cGoStep(cCur+1)">Next →</button>`
    + `</div>`;
}

function cUpdateProgress() {
  const bar = document.getElementById('c-progress-bar');
  if (!bar) return;
  bar.style.width = (C_TOTAL > 0 ? cDone.size/C_TOTAL*100 : 0) + '%';
  if (C_TOTAL > 0 && cDone.size === C_TOTAL) {
    document.getElementById('c-step-content').style.display = 'none';
    document.getElementById('c-completion').style.display = 'block';
  }
}

function cGoStep(i) {
  if (i < 0 || i >= C_TOTAL) return;
  cCur = i;
  cRenderSidebar(); cRenderBoard(); cRenderDetail(); cUpdateProgress();
  const items = document.querySelectorAll('#c-steps-list .step-item');
  if (items[i]) items[i].scrollIntoView({block:'nearest'});
}

function cMarkDone() {
  cDone.add(cCur);
  if (cCur < C_TOTAL-1) cGoStep(cCur+1);
  else { cRenderSidebar(); cUpdateProgress(); }
}

function cPadClick(id) {
  const pi = cPadIdx[id];
  for (let i = 0; i < C_STEPS.length; i++) {
    if (C_STEPS[i].pads.includes(pi)) { cGoStep(i); return; }
  }
}
"#;

const THREED_JS: &str = r#"
// ── 3D viewer ────────────────────────────────────────────────────────────────
let renderer3d = null;
function init3d() {
  if (renderer3d) return;
  if (!GLB_B64) {
    document.getElementById('canvas3d').style.display = 'none';
    document.getElementById('no-3d').style.display = 'block';
    return;
  }
  const canvas = document.getElementById('canvas3d');
  const w = canvas.clientWidth || 700, h = canvas.clientHeight || 500;
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x1a1a2e);
  const camera = new THREE.PerspectiveCamera(60, w/h, 0.1, 10000);
  renderer3d = new THREE.WebGLRenderer({canvas, antialias:true});
  renderer3d.setSize(w, h);
  renderer3d.setPixelRatio(window.devicePixelRatio);
  renderer3d.physicallyCorrectLights = true;
  renderer3d.outputEncoding = THREE.sRGBEncoding;
  scene.add(new THREE.AmbientLight(0xffffff, 1.0));
  const dl = new THREE.DirectionalLight(0xffffff, 1.5); dl.position.set(10,20,10); scene.add(dl);
  const dl2 = new THREE.DirectionalLight(0xffffff, 0.5); dl2.position.set(-10,-10,-5); scene.add(dl2);
  let boardObj = null;
  const loader = new THREE.GLTFLoader();
  // Decode the embedded base64 GLB to an ArrayBuffer and parse it directly.
  // Using loader.parse() (not loader.load() with a data: URI) avoids three.js
  // resolving a resource path against the document, which the browser blocks
  // as a cross-origin file:// load when the page is opened from disk.
  const onLoad = gltf => {
    boardObj = gltf.scene; scene.add(boardObj);
    const box = new THREE.Box3().setFromObject(boardObj);
    const center = new THREE.Vector3(); box.getCenter(center);
    boardObj.position.sub(center);
    const size = box.getSize(new THREE.Vector3());
    const maxDim = Math.max(size.x, size.y, size.z);
    const camDist = Math.abs(maxDim/2/Math.tan(camera.fov*Math.PI/360))*1.8;
    camera.position.set(0, camDist*0.4, camDist);
    camera.lookAt(0,0,0);
  };
  const onError = () => {
    document.getElementById('canvas3d').style.display = 'none';
    document.getElementById('no-3d').style.display = 'block';
  };
  try {
    const bin = atob(GLB_B64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    loader.parse(bytes.buffer, '', onLoad, onError);
  } catch (e) {
    onError();
  }
  let isDragging=false, isPanning=false, prev={x:0,y:0};
  const rot={x:0.3,y:0};
  canvas.addEventListener('mousedown', e => { isDragging=e.button===0; isPanning=e.button===2; prev={x:e.clientX,y:e.clientY}; });
  canvas.addEventListener('mousemove', e => {
    if (!boardObj) return;
    if (isDragging) { rot.y+=(e.clientX-prev.x)*0.01; rot.x+=(e.clientY-prev.y)*0.01; boardObj.rotation.y=rot.y; boardObj.rotation.x=rot.x; }
    else if (isPanning) { camera.position.x-=(e.clientX-prev.x)*0.1; camera.position.y+=(e.clientY-prev.y)*0.1; }
    prev={x:e.clientX,y:e.clientY};
  });
  canvas.addEventListener('mouseup', ()=>{ isDragging=false; isPanning=false; });
  canvas.addEventListener('wheel', e=>{ e.preventDefault(); const d=camera.position.length(); camera.position.setLength(e.deltaY>0?d*1.1:d/1.1); }, {passive:false});
  canvas.addEventListener('contextmenu', e=>e.preventDefault());
  (function animate(){ requestAnimationFrame(animate); renderer3d.render(scene, camera); })();
}
"#;

const TAB_JS: &str = r#"
// ── Tab switching & keyboard nav ─────────────────────────────────────────────
let activeTab = 'assembly';
function switchTab(tab) {
  activeTab = tab;
  document.getElementById('tab-assembly').style.display = tab==='assembly' ? 'grid' : 'none';
  const ctEl = document.getElementById('tab-continuity');
  if (ctEl) ctEl.style.display = tab==='continuity' ? 'grid' : 'none';
  document.getElementById('tab-3d').style.display = tab==='3d' ? 'flex' : 'none';
  document.querySelectorAll('.tab-btn').forEach(b => {
    b.classList.toggle('active', b.textContent.toLowerCase().includes(tab==='3d'?'3d':tab==='continuity'?'continuity':'assembly'));
  });
  if (tab==='3d') init3d();
}

document.addEventListener('keydown', e => {
  if (e.target.tagName === 'INPUT') return;
  if (activeTab === 'assembly') {
    if (e.key==='ArrowRight'||e.key==='ArrowDown') aGoStep(aCur+1);
    if (e.key==='ArrowLeft'||e.key==='ArrowUp') aGoStep(aCur-1);
    if (e.key==='Enter'||e.key===' ') { e.preventDefault(); aMarkDone(); }
  } else if (activeTab === 'continuity') {
    if (e.key==='ArrowRight'||e.key==='ArrowDown') cGoStep(cCur+1);
    if (e.key==='ArrowLeft'||e.key==='ArrowUp') cGoStep(cCur-1);
    if (e.key==='Enter'||e.key===' ') { e.preventDefault(); cMarkDone(); }
  }
});

// Initial render
aRenderSidebar(); aRenderBoard(); aRenderDetail();
cRenderSidebar(); cRenderBoard();
"#;
