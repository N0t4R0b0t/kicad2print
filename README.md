# kicad2print

Convert KiCad PCB designs into 3D-printable substrate models for the **hybrid PCB** construction method — a technique that replaces traditional PCB fabrication with a 3D-printed substrate and copper traces, using either laid copper wire or electroplated copper.

[![Build & Release](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml/badge.svg)](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml)

| 3D Substrate Preview | Assembly Guide |
|:---:|:---:|
| [![3D substrate preview](examples/ps2-serial-mouse-adapter/assembly.png)](https://github.com/N0t4R0b0t/kicad2print/blob/master/examples/ps2-serial-mouse-adapter/ps2-serial-mouse-adapter.stl) | [![Assembly guide](examples/ps2-serial-mouse-adapter/preview.png)](https://github.com/N0t4R0b0t/kicad2print/blob/master/examples/ps2-serial-mouse-adapter/preview.png) |

*Example: [ps2-serial-mouse-adapter](https://github.com/N0t4R0b0t/ps2-serial-mouse-adapter) — a fork of [necroware/ps2-serial-mouse-adapter](https://github.com/necroware/ps2-serial-mouse-adapter). Substrate preview links to the interactive 3D viewer.*

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
2. **Print the substrate** — narrower grooves sized to the actual trace width
3. **Apply conductive primer** to all trace grooves
4. **Electroplate copper** into the grooves using a copper sulfate bath
5. **Test traces**, then solder your components

Produces thinner, more accurate traces and no wire handling. Requires a simple electrolysis setup.

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
| `boardname_preview.html` | Self-contained interactive 3D viewer (no server needed) |
| `boardname_assembly.html` | Step-by-step assembly guide (wire-laying or plating, depending on mode) |

Open either HTML file in any browser — no server needed.

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
| `eyelet_style` | `indent` | `hole` | Via representation (`indent` = dimple, `hole` = through-hole) |
| `eyelet_diameter_mm` | `1.5` | `1.5` | Via hole or dimple diameter |
| `indent_depth_mm` | `0.3` | `0.3` | Dimple depth (indent style only) |
| `pad_hole_diameter_mm` | `0.8` | `0.8` | Minimum component pad hole diameter |
| `substrate_thickness_mm` | `3.0` | `3.0` | Total board thickness |
| `scale_factor` | `0.0` | `0.0` | `0` = true 1:1 scale; `>0` = exact multiplier |
| `output_format` | `stl` | `stl` | `stl`, `3mf`, or `both` |
| `output_dir` | `./output` | `./output` | Output directory |

Settings are merged in order: **built-in defaults → TOML file → CLI flags**.

### Eyelet styles

**`indent`** (copper wire default) — shallow dimples on top and bottom mark via locations. No drilling required. Faster to print and assemble.

**`hole`** (electrolysis default) — full through-holes sized to accept your eyelets. Required when the via hole walls need to be primed and plated.

---

## Tips for printing

- **Layer height:** 0.2 mm works well for most channel widths. Use 0.1 mm for narrow channels (< 1.0 mm).
- **Infill:** 40–60% rectilinear. Higher infill = stiffer board.
- **Material:** PLA is fine for most projects. PETG if you need heat resistance (e.g., near a power section).
- **Orientation:** print flat (board face up). Support is not needed for the trace grooves.
- **First layer:** a good first layer matters — the bottom pad holes need to be clean for component insertion.

---

## MCP server (Claude Desktop)

kicad2print also ships an MCP server that lets Claude Desktop read and make small edits to your KiCad project — useful for quick targeted changes like swapping a footprint, checking the BOM, or running DRC without opening KiCad.

> **Important:** The MCP server is a convenience shortcut, not a replacement for KiCad. For anything beyond small, targeted edits — rerouting, schematic changes, major layout work — you need to open the project in KiCad directly. The MCP server is best used to inspect, tweak, and validate; KiCad is where you design.

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
- **Swap a footprint** — e.g. change an Arduino Uno to a Nano without opening KiCad
- **Check the BOM** — export a CSV of all components and quantities
- **Run DRC** — get a JSON report of design rule violations
- **Convert to substrate** — generate the printable STL/3MF directly from the chat

### Example

```
You:    Scan my project at /home/me/myboard/kicad
Claude: [renders the board, shows BOM, lists all files]

You:    Swap U1 from the Uno footprint to an Arduino Nano
Claude: [searches libraries, reads PCB, replaces footprint,
         writes back, renders updated board, runs DRC]

You:    Convert it to a printable substrate
Claude: [runs kicad2print conversion, returns STL + preview]
```

### Available tools

| Tool | Description |
|---|---|
| `scan_project` | **Start here** — renders board, returns BOM and file list |
| `render_pcb` | Render the board (top / bottom / side views) |
| `run_drc` | Run Design Rules Check — JSON report of violations |
| `export_bom` | Export Bill of Materials as CSV |
| `export_netlist` | Export full component + net connectivity |
| `replace_footprint` | Swap a component footprint in the PCB file |
| `move_component` | Move a component to new coordinates |
| `search_footprint` | Search footprints by name across all libraries |
| `list_footprint_libraries` | List all installed `.pretty` libraries |
| `get_footprint` | Get the raw S-expression for a footprint |
| `export_layer_svg` | Export PCB layers as SVG |
| `convert_pcb` | Convert PCB to 3D-printable substrate (STL/3MF) |
| `read_kicad_file` | Read any `.kicad_pcb` or `.kicad_sch` file |
| `write_kicad_file` | Write a modified design file back to disk |

> **Note:** `render_pcb` requires `kicad-cli` (part of KiCad 9+). Footprint search requires the `kicad-library` package (`sudo pacman -S kicad-library` on Arch/Manjaro).

---

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
