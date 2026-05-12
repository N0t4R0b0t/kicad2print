// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0
// See LICENSE file in the repository root for full details.

//! Assembly guide generator.
//!
//! Produces a self-contained HTML file with an SVG board view for each step.
//! Each step highlights the components or wire traces being placed in that step;
//! everything already placed is shown dimmed.

use crate::config::AssemblyStep;
use crate::pcb::{BoundingBox, PcbData, Point2};
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::Path;
use std::process::Command;

const SVG_W: f64 = 700.0;
const SVG_H: f64 = 500.0;
const PADDING: f64 = 20.0;

struct ViewTransform {
    offset_x: f64,
    offset_y: f64,
    scale: f64,
}

impl ViewTransform {
    fn new(bbox: &BoundingBox) -> Self {
        let board_w = bbox.width();
        let board_h = bbox.height();
        let sx = (SVG_W - 2.0 * PADDING) / board_w.max(0.001);
        let sy = (SVG_H - 2.0 * PADDING) / board_h.max(0.001);
        let scale = sx.min(sy);
        ViewTransform {
            offset_x: PADDING - bbox.min_x * scale,
            offset_y: PADDING + (bbox.max_y) * scale,
            scale,
        }
    }

    // PCB coords are Y-up; SVG is Y-down, so flip Y.
    fn px(&self, p: Point2) -> (f64, f64) {
        (
            self.offset_x + p.x * self.scale,
            self.offset_y - p.y * self.scale,
        )
    }
}

/// Build the default steps when the user provides none.
/// Step 1: all components; Step 2: F.Cu wires; Step 3: B.Cu wires.
fn default_steps(pcb: &PcbData) -> Vec<AssemblyStep> {
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

fn render_svg(pcb: &PcbData, step_idx: usize, steps: &[AssemblyStep]) -> String {
    let step = &steps[step_idx];

    let bbox = match &pcb.outline {
        Some(o) => o.bbox,
        None => {
            // Derive bbox from all points
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

    let _ = write!(svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_W}" height="{SVG_H}" style="background:#1a2a1a;border-radius:8px">"#);

    // Board outline
    if let Some(outline) = &pcb.outline {
        let pts: Vec<String> = outline.vertices.iter().map(|&p| {
            let (x, y) = vt.px(p);
            format!("{x:.1},{y:.1}")
        }).collect();
        let _ = write!(svg, "<polygon points=\"{}\" fill=\"#1e3a1e\" stroke=\"#44aa44\" stroke-width=\"1.5\"/>", pts.join(" "));
    }

    // Components already placed in earlier steps (dimmed)
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
            ("#223322", "#334433", "0.4")
        };

        // Draw pads
        for pad in &fp.pads {
            let (px, py) = vt.px(pad.center);
            let r = (pad.drill * vt.scale / 2.0).max(3.0);
            let _ = write!(svg, r#"<circle cx="{px:.1}" cy="{py:.1}" r="{r:.1}" fill="{pad_color}" opacity="{opacity}"/>"#);
        }

        // Component label
        let _ = write!(svg, r#"<text x="{cx:.1}" y="{cy:.1}" fill="{label_color}" font-size="9" font-family="monospace" text-anchor="middle" opacity="{opacity}">{}</text>"#,
            html_escape(&fp.reference));
    }

    // Determine which prior layers have been wired
    let fcu_done = steps[..step_idx].iter().any(|s| s.wire_layer.as_deref() == Some("F.Cu"));
    let bcu_done = steps[..step_idx].iter().any(|s| s.wire_layer.as_deref() == Some("B.Cu"));
    let show_fcu = step.wire_layer.as_deref() == Some("F.Cu");
    let show_bcu = step.wire_layer.as_deref() == Some("B.Cu");

    // F.Cu traces
    for trace in &pcb.traces_fcu {
        let (x1, y1) = vt.px(trace.start);
        let (x2, y2) = vt.px(trace.end);
        let (color, width, opacity) = if show_fcu {
            ("#ff4444", "2.5", "1.0")
        } else if fcu_done {
            ("#883333", "1.5", "0.5")
        } else {
            ("#331111", "1.0", "0.3")
        };
        let _ = write!(svg, r#"<line x1="{x1:.1}" y1="{y1:.1}" x2="{x2:.1}" y2="{y2:.1}" stroke="{color}" stroke-width="{width}" opacity="{opacity}" stroke-linecap="round"/>"#);
    }

    // B.Cu traces
    for trace in &pcb.traces_bcu {
        let (x1, y1) = vt.px(trace.start);
        let (x2, y2) = vt.px(trace.end);
        let (color, width, opacity) = if show_bcu {
            ("#4488ff", "2.5", "1.0")
        } else if bcu_done {
            ("#223366", "1.5", "0.5")
        } else {
            ("#111133", "1.0", "0.3")
        };
        let _ = write!(svg, r#"<line x1="{x1:.1}" y1="{y1:.1}" x2="{x2:.1}" y2="{y2:.1}" stroke="{color}" stroke-width="{width}" opacity="{opacity}" stroke-linecap="round"/>"#);
    }

    // Vias
    let vias_done = steps[..step_idx].iter().any(|s| s.wire_layer.is_none() && s.components.is_empty() && s.name.to_lowercase().contains("via"));
    let vias_active = step.name.to_lowercase().contains("via") && step.components.is_empty() && step.wire_layer.is_none();
    for via in &pcb.vias {
        let (cx, cy) = vt.px(via.center);
        let r = (via.drill * vt.scale / 2.0).max(3.0);
        let (color, opacity) = if vias_active {
            ("#ffdd00", "1.0")
        } else if vias_done {
            ("#665500", "0.6")
        } else {
            ("#333300", "0.3")
        };
        let _ = write!(svg, r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="none" stroke="{color}" stroke-width="1.5" opacity="{opacity}"/>"#);
    }

    svg.push_str("</svg>");
    svg
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn build_parts_table(pcb: &PcbData, step: &AssemblyStep) -> String {
    if step.components.is_empty() {
        return String::new();
    }

    let mut rows = String::new();
    for refdes in &step.components {
        let fp = pcb.footprints.iter().find(|f| &f.reference == refdes);
        let value = fp.map(|f| f.value.as_str()).unwrap_or("—");
        let _ = write!(rows,
            r#"<tr><td style="padding:4px 10px;color:#00ff88;font-family:monospace">{}</td><td style="padding:4px 10px;color:#cccccc">{}</td></tr>"#,
            html_escape(refdes), html_escape(value)
        );
    }
    format!(r#"<table style="border-collapse:collapse;font-size:13px;margin-top:12px">
  <tr><th style="padding:4px 10px;color:#888;text-align:left">Ref</th><th style="padding:4px 10px;color:#888;text-align:left">Value</th></tr>
  {rows}
</table>"#)
}

/// Run kicad-cli to export a GLB and return it base64-encoded.
/// Returns None if kicad-cli is unavailable or the export fails.
fn export_glb_base64(pcb_input: &Path) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!(
        "kicad2print_{}.glb",
        pcb_input.file_stem()?.to_str()?
    ));
    let status = Command::new("kicad-cli")
        .args(["pcb", "export", "glb",
               "--output", tmp.to_str()?,
               "--force",
               "--subst-models",
               pcb_input.to_str()?])
        .env("DISPLAY", "")
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    Some(BASE64.encode(&bytes))
}

pub fn write(pcb: &PcbData, pcb_input: &Path, steps_cfg: &[AssemblyStep], stem: &str, path: &Path) -> Result<()> {
    let steps: Vec<AssemblyStep> = if steps_cfg.is_empty() {
        default_steps(pcb)
    } else {
        steps_cfg.to_vec()
    };

    if steps.is_empty() {
        return Ok(());
    }

    // Pre-render all SVGs and step bodies into JS arrays
    let mut svgs_js = String::new();
    let mut titles_js = String::new();
    let mut instructions_js = String::new();
    let mut parts_js = String::new();

    for (i, step) in steps.iter().enumerate() {
        let svg = render_svg(pcb, i, &steps);
        let parts = build_parts_table(pcb, step);
        let sep = if i > 0 { "," } else { "" };
        let _ = write!(svgs_js, "{sep}`{}`", svg.replace('`', "\\`").replace("${", "\\${"));
        let _ = write!(titles_js, "{sep}`{}`", html_escape(&step.name).replace('`', "\\`"));
        let _ = write!(instructions_js, "{sep}`{}`", html_escape(&step.instruction).replace('`', "\\`"));
        let _ = write!(parts_js, "{sep}`{}`", parts.replace('`', "\\`").replace("${", "\\${"));
    }

    let total = steps.len();
    let glb_b64 = export_glb_base64(pcb_input);

    let glb_data_uri_js = match &glb_b64 {
        Some(b64) => format!(r#""data:model/gltf-binary;base64,{b64}""#),
        None => "null".to_string(),
    };

    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Assembly Guide — {stem}</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: #111; color: #eee; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; min-height: 100vh; display: flex; flex-direction: column; align-items: center; padding: 24px 16px; }}
  h1 {{ font-size: 20px; color: #aaddaa; margin-bottom: 8px; }}
  .tabs {{ display: flex; gap: 0; margin-bottom: 16px; border: 1px solid #44aa44; border-radius: 6px; overflow: hidden; }}
  .tab-btn {{ background: #111; border: none; color: #aaffaa; padding: 8px 24px; font-size: 14px; cursor: pointer; transition: background 0.15s; }}
  .tab-btn.active {{ background: #1e3a1e; color: #00ff88; font-weight: 600; }}
  .tab-btn:hover:not(.active) {{ background: #1a2a1a; }}
  #tab-guide {{ display: flex; flex-direction: column; align-items: center; width: 100%; }}
  #tab-3d {{ display: none; width: 100%; max-width: 700px; }}
  #canvas3d {{ width: 100%; height: 500px; border-radius: 8px; background: #2a2a2a; display: block; }}
  #step-counter {{ font-size: 13px; color: #666; margin-bottom: 20px; }}
  #board-view {{ max-width: 700px; width: 100%; }}
  #board-view svg {{ width: 100%; height: auto; }}
  #step-title {{ font-size: 18px; font-weight: 600; color: #00ff88; margin: 16px 0 8px; }}
  #instruction {{ font-size: 14px; color: #bbb; line-height: 1.6; max-width: 700px; }}
  #parts {{ max-width: 700px; width: 100%; }}
  .nav {{ display: flex; gap: 12px; margin-top: 24px; }}
  button {{ background: #1e3a1e; border: 1px solid #44aa44; color: #aaffaa; padding: 10px 28px; border-radius: 6px; font-size: 15px; cursor: pointer; transition: background 0.15s; }}
  button:hover {{ background: #2a5a2a; }}
  button:disabled {{ opacity: 0.3; cursor: default; }}
  .progress {{ display: flex; gap: 6px; margin-top: 20px; flex-wrap: wrap; max-width: 700px; }}
  .dot {{ width: 10px; height: 10px; border-radius: 50%; background: #333; cursor: pointer; transition: background 0.15s; }}
  .dot.done {{ background: #336633; }}
  .dot.active {{ background: #00ff88; }}
  .legend {{ display: flex; gap: 16px; margin-top: 12px; font-size: 12px; color: #888; flex-wrap: wrap; }}
  .legend-item {{ display: flex; align-items: center; gap: 5px; }}
  .legend-swatch {{ width: 16px; height: 4px; border-radius: 2px; }}
  #view3d-controls {{ font-size: 12px; color: #666; margin-top: 8px; text-align: center; }}
  #no-3d {{ color: #666; font-size: 14px; padding: 40px; text-align: center; }}
</style>
</head>
<body>
<h1>Assembly Guide — {stem}</h1>
<div class="tabs">
  <button class="tab-btn active" onclick="switchTab('guide')">Assembly Steps</button>
  <button class="tab-btn" onclick="switchTab('3d')">3D Model</button>
</div>

<div id="tab-guide">
  <div id="step-counter"></div>
  <div id="board-view"></div>
  <div class="legend">
    <div class="legend-item"><div class="legend-swatch" style="background:#00ff88"></div>This step</div>
    <div class="legend-item"><div class="legend-swatch" style="background:#336633"></div>Already placed</div>
    <div class="legend-item"><div class="legend-swatch" style="background:#ff4444"></div>F.Cu wire</div>
    <div class="legend-item"><div class="legend-swatch" style="background:#4488ff"></div>B.Cu wire</div>
    <div class="legend-item"><div class="legend-swatch" style="background:#ffdd00;height:2px;background:none;border:2px solid #ffdd00;border-radius:50%;width:10px"></div>Via</div>
  </div>
  <div id="step-title"></div>
  <div id="instruction"></div>
  <div id="parts"></div>
  <div class="nav">
    <button id="btn-prev" onclick="go(-1)">← Prev</button>
    <button id="btn-next" onclick="go(1)">Next →</button>
  </div>
  <div class="progress" id="progress"></div>
</div>

<div id="tab-3d">
  <canvas id="canvas3d"></canvas>
  <div id="view3d-controls">Drag: Rotate &nbsp;|&nbsp; Scroll: Zoom &nbsp;|&nbsp; Right-drag: Pan</div>
  <div id="no-3d" style="display:none">3D model unavailable — kicad-cli GLB export failed or kicad-cli is not installed.</div>
</div>

<script src="https://cdnjs.cloudflare.com/ajax/libs/three.js/r128/three.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/three@0.128.0/examples/js/loaders/GLTFLoader.js"></script>
<script>
const SVGS = [{svgs_js}];
const TITLES = [{titles_js}];
const INSTRUCTIONS = [{instructions_js}];
const PARTS = [{parts_js}];
const TOTAL = {total};
const GLB_DATA_URI = {glb_data_uri_js};
let cur = 0;

function render() {{
  document.getElementById('board-view').innerHTML = SVGS[cur];
  document.getElementById('step-title').textContent = `Step ${{cur+1}}: ${{TITLES[cur]}}`;
  document.getElementById('instruction').innerHTML = INSTRUCTIONS[cur];
  document.getElementById('parts').innerHTML = PARTS[cur];
  document.getElementById('step-counter').textContent = `Step ${{cur+1}} of ${{TOTAL}}`;
  document.getElementById('btn-prev').disabled = cur === 0;
  document.getElementById('btn-next').disabled = cur === TOTAL - 1;
  document.querySelectorAll('.dot').forEach((d, i) => {{
    d.className = 'dot' + (i < cur ? ' done' : i === cur ? ' active' : '');
  }});
}}

function go(dir) {{
  cur = Math.max(0, Math.min(TOTAL-1, cur + dir));
  render();
}}

const prog = document.getElementById('progress');
for (let i = 0; i < TOTAL; i++) {{
  const d = document.createElement('div');
  d.className = 'dot';
  d.title = `Step ${{i+1}}`;
  d.onclick = () => {{ cur = i; render(); }};
  prog.appendChild(d);
}}

document.addEventListener('keydown', e => {{
  if (document.getElementById('tab-guide').style.display !== 'none') {{
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') go(1);
    if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') go(-1);
  }}
}});

// --- 3D viewer ---
let renderer3d = null;

function init3d() {{
  if (renderer3d) return;

  if (!GLB_DATA_URI) {{
    document.getElementById('canvas3d').style.display = 'none';
    document.getElementById('view3d-controls').style.display = 'none';
    document.getElementById('no-3d').style.display = 'block';
    return;
  }}

  const canvas = document.getElementById('canvas3d');
  const w = canvas.clientWidth || 700, h = canvas.clientHeight || 500;

  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x2a2a2a);
  const camera = new THREE.PerspectiveCamera(60, w / h, 0.1, 10000);
  renderer3d = new THREE.WebGLRenderer({{ canvas, antialias: true }});
  renderer3d.setSize(w, h);
  renderer3d.setPixelRatio(window.devicePixelRatio);
  renderer3d.physicallyCorrectLights = true;
  renderer3d.outputEncoding = THREE.sRGBEncoding;

  scene.add(new THREE.AmbientLight(0xffffff, 1.0));
  const dirLight = new THREE.DirectionalLight(0xffffff, 1.5);
  dirLight.position.set(10, 20, 10);
  scene.add(dirLight);
  const dirLight2 = new THREE.DirectionalLight(0xffffff, 0.5);
  dirLight2.position.set(-10, -10, -5);
  scene.add(dirLight2);
  scene.add(new THREE.GridHelper(200, 20, 0x444444, 0x222222));

  let boardObj = null;
  const center = new THREE.Vector3();

  const loader = new THREE.GLTFLoader();
  loader.load(GLB_DATA_URI, gltf => {{
    boardObj = gltf.scene;
    scene.add(boardObj);

    const box = new THREE.Box3().setFromObject(boardObj);
    box.getCenter(center);
    boardObj.position.sub(center);

    const size = box.getSize(new THREE.Vector3());
    const maxDim = Math.max(size.x, size.y, size.z);
    const fov = camera.fov * Math.PI / 180;
    const camDist = Math.abs(maxDim / 2 / Math.tan(fov / 2)) * 1.8;
    camera.position.set(0, camDist * 0.4, camDist);
    camera.lookAt(0, 0, 0);
  }}, undefined, err => {{
    console.error('GLTFLoader error', err);
    document.getElementById('canvas3d').style.display = 'none';
    document.getElementById('view3d-controls').style.display = 'none';
    document.getElementById('no-3d').style.display = 'block';
  }});

  let isDragging = false, isPanning = false;
  let prev = {{ x: 0, y: 0 }};
  const rot = {{ x: 0.3, y: 0 }};

  canvas.addEventListener('mousedown', e => {{
    isDragging = e.button === 0; isPanning = e.button === 2;
    prev = {{ x: e.clientX, y: e.clientY }};
  }});
  canvas.addEventListener('mousemove', e => {{
    if (!boardObj) return;
    if (isDragging) {{
      rot.y += (e.clientX - prev.x) * 0.01;
      rot.x += (e.clientY - prev.y) * 0.01;
      boardObj.rotation.y = rot.y;
      boardObj.rotation.x = rot.x;
    }} else if (isPanning) {{
      camera.position.x -= (e.clientX - prev.x) * 0.1;
      camera.position.y += (e.clientY - prev.y) * 0.1;
    }}
    prev = {{ x: e.clientX, y: e.clientY }};
  }});
  canvas.addEventListener('mouseup', () => {{ isDragging = false; isPanning = false; }});
  canvas.addEventListener('wheel', e => {{
    e.preventDefault();
    const dist = camera.position.length();
    const newDist = e.deltaY > 0 ? dist * 1.1 : dist / 1.1;
    camera.position.setLength(newDist);
  }}, {{ passive: false }});
  canvas.addEventListener('contextmenu', e => e.preventDefault());

  (function animate() {{
    requestAnimationFrame(animate);
    renderer3d.render(scene, camera);
  }})();
}}

function switchTab(tab) {{
  const isGuide = tab === 'guide';
  document.getElementById('tab-guide').style.display = isGuide ? 'flex' : 'none';
  document.getElementById('tab-3d').style.display = isGuide ? 'none' : 'block';
  document.querySelectorAll('.tab-btn').forEach((b, i) => {{
    b.classList.toggle('active', (i === 0) === isGuide);
  }});
  if (!isGuide) init3d();
}}

render();
</script>
</body>
</html>"#);

    let mut file = std::fs::File::create(path)?;
    file.write_all(html.as_bytes())?;
    Ok(())
}
