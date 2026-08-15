# kicad2print

Convert KiCad PCB designs into 3D-printable substrate models for the **hybrid PCB** construction method — a technique that replaces traditional PCB fabrication with a 3D-printed substrate and copper traces, using either laid copper wire or electroplated copper.

[![Build & Release](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml/badge.svg)](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml)

<p align="center">
  <img src="examples/ps2-serial-mouse-adapter/guide-demo.gif" alt="kicad2print unified build guide demo — assembly steps, interactive continuity test, and 3D preview" width="800"/>
  <br/>
  <em>The generated build guide: assembly steps, an interactive continuity test that pulses probe dots on every pad of the selected net, and an embedded 3D model — all in one self-contained HTML file.</em>
</p>

*Example: [ps2-serial-mouse-adapter](https://github.com/N0t4R0b0t/ps2-serial-mouse-adapter) — view the [interactive 3D substrate model](https://github.com/N0t4R0b0t/kicad2print/blob/master/examples/ps2-serial-mouse-adapter/ps2-serial-mouse-adapter.stl).*

---

## What is the hybrid PCB method?

Instead of sending your board to a fab house, you print the substrate on an FDM printer and add copper traces yourself. kicad2print supports two construction modes:

### Copper wire traces (`--mode copper-wire`, default)

1. **Design your PCB normally in KiCad**
2. **Print the substrate** — grooved channels for traces, holes for pads and vias
3. **Lay 30 AWG copper wire** into each channel
4. **Press copper eyelets** into via holes to bridge top and bottom layers
5. **Solder your components**

No chemicals, no etching, no minimum order. A functional PCB in a few hours.

### Electroplated copper (`--mode electrolysis`)

1. **Design your PCB normally in KiCad**
2. **Print the substrate** — narrower grooves sized to the actual trace width — plus the **snap-on stencil** kicad2print generates alongside it
3. **Apply conductive primer** — snap on the stencil and squeegee paint so it lands only in the grooves (optionally have the stencil also lay down a temporary bus that shorts every trace for plating)
4. **Electroplate copper** into the grooves using a copper sulfate bath
5. **Test traces** (grind off the bus first if you used one), then solder your components

Produces thinner, more accurate traces and no wire handling. The auto-generated stencil keeps paint out of the flat areas (minimal cleanup), and can optionally build the "every net must reach the cathode" plating bus for you (`--plating-bus`). Requires a simple electrolysis setup — see [docs/ELECTROLYSIS.md](docs/ELECTROLYSIS.md) for the full end-to-end procedure: seed paints, bath chemistry, sourcing, plating run, and installing eyelets before plating so they become part of the copper layer.

---

**kicad2print** handles the substrate step: it takes your `.kicad_pcb` file and produces the STL/3MF model ready to slice and print, plus an HTML assembly guide tailored to whichever mode you choose.

---

## Installation

Download the binary for your platform from the [Releases page](https://github.com/N0t4R0b0t/kicad2print/releases).

**Linux:**
```bash
chmod +x kicad2print-linux-x86_64
sudo mv kicad2print-linux-x86_64 /usr/local/bin/kicad2print
```

**Windows:** download `kicad2print-windows-x86_64.exe` and place it on your `PATH`.

**Snapshot build** (latest main branch): download from the [`snapshot` release](https://github.com/N0t4R0b0t/kicad2print/releases/tag/snapshot).

### Build from source

```bash
git clone https://github.com/N0t4R0b0t/kicad2print.git
cd kicad2print
cargo build --release
# binary at: target/release/kicad2print
```

---

## Usage

```bash
# Basic conversion — copper wire mode (default)
kicad2print my_board.kicad_pcb

# Electrolysis mode — narrower channels, plating assembly guide
kicad2print my_board.kicad_pcb --mode electrolysis

# With a config file (copy a preset as a starting point)
kicad2print my_board.kicad_pcb --config presets/electrolysis.toml

# Override individual settings on top of a mode
kicad2print my_board.kicad_pcb --mode electrolysis --channel-width 0.5

# Generate both STL and 3MF
kicad2print my_board.kicad_pcb --format both

# Auto-open the HTML 3D preview after conversion
kicad2print my_board.kicad_pcb --view
```

### Output files

Each run produces the following in `--output-dir` (default `./output/`):

| File | Description |
|---|---|
| `boardname.stl` | Binary STL for slicers (when format = `stl` or `both`) |
| `boardname.3mf` | 3MF with metadata (when format = `3mf` or `both`) |
| `boardname_guide.html` | Unified build guide — tabbed view with **assembly steps**, **continuity test**, and an interactive **3D preview**, tailored to the selected mode |

Open the guide in any browser — no server needed.

### What's in the unified guide

- **Assembly tab** — step-by-step instructions for the selected mode (wire-laying or plating), with images and the BOM.
- **Continuity test tab** — an interactive SVG board diagram. Pick a net from the sidebar and probe dots pulse on every pad that should be electrically connected, so you can verify continuity with a multimeter after wiring or plating.
  - Through-hole **and SMD pads** (e.g. SOIC, SOT, QFN) are included — anything with a net name gets a probe dot.
  - **Power-rail nets** that KiCad didn't name in the schematic (the `unconnected-*` nets KiCad auto-creates when 2+ pads share a node) are detected and listed with a ⚠ marker, so power and ground continuity can still be verified.
- **3D preview tab** — the same interactive three.js viewer, embedded directly in the guide.

---

## Configuration

### Quick start with a preset

Copy one of the presets from this repo as your starting point:

```bash
# Copper wire traces (default settings)
cp presets/copper-wire.toml kicad2print.toml

# Electroplated copper
cp presets/electrolysis.toml kicad2print.toml
```

Then edit `kicad2print.toml` to taste and run:

```bash
kicad2print my_board.kicad_pcb --config kicad2print.toml
```

Or skip the file entirely and use `--mode` for the preset defaults:

```bash
kicad2print my_board.kicad_pcb --mode electrolysis
```

### All settings

| Setting | Copper wire default | Electrolysis default | Description |
|---|---|---|---|
| `mode` | `copper-wire` | `electrolysis` | Selects assembly guide style |
| `channel_width_mm` | `1.2` | `0.7` | Groove width — wire diameter or trace width |
| `channel_depth_mm` | `0.5` | `0.5` | Groove depth |
| `channel_profile` | `rect` | `trapezoid` | Groove cross-section: `rect`, `trapezoid`, or `vee` |
| `channel_floor_width_mm` | `0.4` | `0.4` | Groove floor width for `trapezoid` (opening stays at `channel_width_mm`) |
| `taper_slice_height_mm` | `0.2` | `0.2` | Step height for sloped walls — set to your slicer's layer height |
| `via_style` | `straight` | `straight` | Barrel shape: `straight`, or `cone` for eyelet-free plated holes |
| `cone_angle_deg` | `45.0` | `45.0` | Countersink wall angle from the board surface (`cone` only) |
| `throat_height_mm` | `0.4` | `0.4` | Straight section where the two cones meet (`cone` only) |
| `min_rim_mm` | `0.3` | `0.3` | Material kept between a cone mouth and foreign-net copper (`cone` only) |
| `eyelet_diameter_mm` | `1.5` | `1.5` | Minimum via bore diameter |
| `pad_hole_diameter_mm` | `0.8` | `0.8` | Minimum component pad hole diameter |
| `substrate_thickness_mm` | `3.0` | `3.0` | Total board thickness |
| `scale_factor` | `0.0` | `0.0` | `0` = true 1:1 scale; `>0` = exact multiplier |
| `output_format` | `stl` | `stl` | `stl`, `3mf`, or `both` |
| `output_dir` | `./output` | `./output` | Output directory |

Settings are merged in order: **built-in defaults → TOML file → CLI flags**.

> `eyelet_style`, `indent_depth_mm` and `--no-via-indents` are deprecated and
> inert. They never changed the generated geometry — vias have always been cut
> as full through-holes — and the tool now says so when they are set. Use
> `via_style` instead.

### Groove profiles

**`rect`** (default) — vertical walls and a flat floor. What wire mode wants, so the wire seats flat against the floor.

**`trapezoid`** / **`vee`** — sloped walls. A `trapezoid` descends to a deliberate flat floor of `channel_floor_width_mm`; a `vee` ignores that setting and converges, letting the slicer truncate it where the groove gets too narrow to extrude. These matter for electroplating. A square-bottomed groove plates unevenly: current density is highest at the top corners and lowest at the floor centre, so copper grows inward from the walls and can seal over the opening — leaving a void — before the floor is covered. Removing the corner lets the groove fill from the bottom up. Sloped walls also print better on a 0.4 mm nozzle, most of all on the underside, where a square groove has to be closed off by bridging a flat ceiling. The trade-off is copper cross-section: a `vee` carries roughly half of what a `rect` does at the same width and depth, so prefer `trapezoid` where current matters.

### Via styles

**`straight`** (default) — a plain bore. The two copper layers are otherwise completely separate, and you cannot get a brush down a small hole through a couple of millimetres of plastic, so the barrel never plates. Bridge the layers with a pressed-in eyelet, or — usually far less painful — a snipped component lead soldered on both faces.

**`cone`** — countersunk from both faces, meeting at a short straight throat. Every point of the barrel is then in line of sight from one face or the other, so seed paint can actually be applied and plating grows from both mouths toward the middle. No eyelet, no flange to trim. The bottom cone doubles as a solder cup, which is what makes a top-side trace solderable from underneath.

Cone mouths are much wider than the bore — a 0.8 mm hole with 45° walls through a 2.2 mm board wants a ~3 mm crater per face — so on fine pitches they are shrunk automatically to keep `min_rim_mm` clear of other nets, and holes with no room stay straight. The run reports how many.

The countersink is built from the same band stack that forms the trace grooves, so it works with every groove profile. Cone depth is bounded by `channel_depth_mm` for that reason, with `throat_height_mm` applying whichever is tighter.

### Mesh validation

Every generated mesh is checked for being a closed, consistently-wound solid, and the result is reported. This matters because the failure modes are invisible in a 3D preview: an open surface is what lets a slicer fill a through-hole solid or render the board as a featureless plaque. The check also reports genus, which verifies the through-holes are topologically present. Don't print past a warning.

---

## Tips for printing

- **Layer height:** 0.2 mm works well for most channel widths. Use 0.1 mm for narrow channels (< 1.0 mm).
- **Infill:** 40–60% rectilinear. Higher infill = stiffer board.
- **Material:** PLA is fine for most projects. PETG if you need heat resistance (e.g., near a power section).
- **Orientation:** print flat (board face up). Support is not needed for the trace grooves.
- **Sloped walls:** if you use `channel_profile = "trapezoid"`/`"vee"` or `via_style = "cone"`, set `taper_slice_height_mm` to your layer height. Sloped walls are built as a stack of thin bands, and at layer height the printed part is identical to a smooth ramp.
- **First layer:** a good first layer matters — the bottom pad holes need to be clean for component insertion.

---

## MCP server (Claude Desktop)

kicad2print also ships an MCP server that lets Claude Desktop read and make small edits to your KiCad project — useful for quick targeted changes like swapping a footprint, checking the BOM, or running DRC without opening KiCad.

> **These tools will not replace KiCad and can make your board worse.** Read [docs/MCP_KICAD_TOOLS.md](docs/MCP_KICAD_TOOLS.md) before using them on a board you care about. Commit your work before starting any AI editing session.

### Setup

Add to `~/.config/Claude/claude_desktop_config.json` (Linux) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "kicad2print": {
      "command": "/usr/local/bin/kicad2print",
      "args": ["--mcp"]
    }
  }
}
```

Restart Claude Desktop. The KiCad tools will appear automatically.

### What you can do

- **Scan a project** — get a rendered board image, full BOM, and file list in one shot
- **Inspect before routing** — query pad positions, check net names, verify clearances before touching traces
- **Swap a footprint** — e.g. change an Arduino Uno to a Nano without opening KiCad
- **Check the BOM** — export a CSV of all components and quantities
- **Run DRC** — get a JSON report of violations with a board render
- **Convert to substrate** — generate the printable STL/3MF directly from the chat

### Example

```
You:    Scan my project at /home/me/myboard/kicad
Claude: [renders the board, shows BOM, lists all files]

You:    What nets are on this board and which pads carry VBUS?
Claude: [calls list_nets — returns every net name and connected pad list]

You:    Check if a 0.4mm trace from (97,63) to (113,63) on B.Cu is safe
Claude: [calls check_trace_clearance — reports any pad collisions before routing]

You:    Convert it to a printable substrate
Claude: [runs kicad2print conversion, returns STL + preview]
```

### Key tools

| Tool | Description |
|---|---|
| `scan_project` | **Start here** — renders board, returns BOM and file list |
| `list_nets` | All nets with connected pads — **call before any edit** to get correct net names |
| `get_net_for_pad` | Net name and absolute position of one pad |
| `query_pads_in_region` | All pads in a bounding box — inspect an area before routing |
| `query_traces_in_region` | All trace segments in a bounding box — pairs with `query_pads_in_region` |
| `check_trace_clearance` | Collision check vs. pads *and* existing traces — run before `add_trace`, or pass `add_trace(check: true)` |
| `delete_trace` | Remove trace segments by net/layer/uuid/region — the fix for bad routing, with `dry_run` support |
| `verify_connectivity` | Confirm two pads are physically wired by existing traces/vias |
| `add_power_symbol` | Place a power net symbol with correct `lib_symbols` definition |
| `render_pcb` | Render the board (top / bottom / side views) |
| `run_drc` | Design Rules Check — JSON report + board render |
| `export_layer_svg` | Export copper layers as SVG + PNG image |
| `replace_footprint` | Swap a component footprint in the PCB file |
| `convert_pcb` | Convert PCB to 3D-printable substrate (STL/3MF) |

See [docs/MCP_KICAD_TOOLS.md](docs/MCP_KICAD_TOOLS.md) for the full tool list, risk levels, recommended workflow, and known limitations.

> **Note:** `render_pcb` and `export_layer_svg` require `kicad-cli` (part of KiCad 9+). `export_layer_svg` PNG output requires `rsvg-convert` (`librsvg`). Footprint search requires the `kicad-library` package (`sudo pacman -S kicad-library` on Arch/Manjaro).

---

## Claude Code plugin (recommended for agents)

The MCP server above also installs as a [Claude Code plugin](https://code.claude.com/docs/en/plugins), which — unlike Claude Desktop — can bundle custom subagents alongside the MCP tools. This repo self-hosts its own plugin marketplace.

**Prerequisite:** the `kicad2print` binary must already be installed and on your `PATH` (see [Installation](#installation) above) — the plugin points at it by name, it doesn't bundle the binary.

```
/plugin marketplace add N0t4R0b0t/kicad2print
/plugin install kicad2print@kicad2print
```

Claude Desktop has no concept of agents — it only reads the plain MCP config shown above. Use the Claude Code plugin if you want the bundled agent(s) as well as the MCP tools.

The plugin also bundles six **skills** — safe editing, PCB routing, schematic editing, project creation, and two for contributors working on kicad2print itself. They load automatically when the task matches. See [docs/MCP_KICAD_TOOLS.md](docs/MCP_KICAD_TOOLS.md#bundled-skills).

The bundled agent is **`kicad-worker`** (`agents/kicad-worker.md`), which runs the tool-heavy calls — DRC/ERC, reads and greps, position and net lookups, renders, specified edits — on a cheaper model and reports back a summary. These tools return large payloads, and keeping them out of the main conversation leaves room for the actual design reasoning. Give it decided work, not decisions. See [docs/MCP_KICAD_TOOLS.md](docs/MCP_KICAD_TOOLS.md#the-kicad-worker-subagent).

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `Failed to read KiCad file` | Wrong path or unreadable file | Check the path; confirm the file is a `.kicad_pcb`, not a `.kicad_sch` |
| `No board outline found` | Missing geometry on the `Edge.Cuts` layer | Add a board outline in KiCad (Place → Line on Edge.Cuts) |
| Channels printed too narrow to fit wire | Source traces narrower than `channel_width_mm` and `scale_factor` is non-zero | Set `scale_factor = 0` to auto-scale, or increase `scale_factor` |
| Eyelets won't press in | `eyelet_diameter_mm` smaller than your eyelets | Measure your eyelets with calipers and update the setting |
| MCP `render_pcb` fails | `kicad-cli` not on `PATH` | Install KiCad 9+ |
| MCP `search_footprint` returns nothing | KiCad footprint libraries not installed | Install `kicad-library` (e.g. `sudo pacman -S kicad-library` on Arch/Manjaro) |

## How the conversion works

```
.kicad_pcb
    │
    ├─ parser/sexp.rs     Tokenize S-expressions → SexpNode tree
    ├─ parser/kicad.rs    Walk tree → PcbData (traces, vias, pads, outline, cutouts)
    ├─ autoscale.rs       Scale board so narrowest trace fills a channel
    ├─ geometry/          Tessellate 3D substrate mesh with grooves and holes
    ├─ export/stl.rs      Write binary STL
    ├─ export/threemf.rs  Write 3MF (ZIP + XML)
    └─ export/html.rs     Write self-contained three.js preview
```

**Coordinate convention:** KiCad uses Y-down; kicad2print negates Y at parse time so all geometry operates in standard Y-up coordinates.

---

## Building & development

```bash
cargo build           # debug
cargo build --release # optimised
cargo test            # unit tests
cargo clippy          # lints
cargo fmt             # format
```

---

## License

AGPL-3.0 — see [LICENSE](LICENSE).
