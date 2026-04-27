# kicad2print

Convert KiCad PCB designs into 3D-printable substrate models, and work on KiCad projects interactively through Claude Desktop via an MCP server.

[![Build & Release](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml/badge.svg)](https://github.com/N0t4R0b0t/kicad2print/actions/workflows/release.yml)

## What it does

**As a CLI tool** — takes a `.kicad_pcb` file and generates STL/3MF models of the PCB substrate for the "hybrid PCB" construction method: 3D-printed substrates with copper wire traces and copper eyelets instead of traditional PCB fabrication.

**As an MCP server** — integrates with Claude Desktop to let you work on KiCad projects conversationally: read and write PCB files, render real 3D previews, run DRC, look up footprints, and convert to printable substrates.

---

## Installation

### From a release

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

## CLI usage

```bash
# Basic conversion — outputs STL/3MF + HTML preview to ./output/
kicad2print my_board.kicad_pcb

# With a config file
kicad2print my_board.kicad_pcb --config my_settings.toml

# Override specific settings
kicad2print my_board.kicad_pcb --channel-width 0.8 --eyelet-style hole

# Generate both STL and 3MF
kicad2print my_board.kicad_pcb --format both

# Auto-open the HTML 3D preview after conversion
kicad2print my_board.kicad_pcb --view
```

### Configuration file (`kicad2print.toml`)

```toml
channel_width_mm       = 1.2      # Width of wire trace grooves
channel_depth_mm       = 0.5      # Depth of trace grooves
eyelet_style           = "indent" # "hole" or "indent"
eyelet_diameter_mm     = 1.5      # Via hole / dimple diameter
indent_depth_mm        = 0.3      # Dimple depth (indent style only)
pad_hole_diameter_mm   = 0.8      # Component pad through-hole diameter
substrate_thickness_mm = 3.0      # Total board thickness
scale_factor           = 0.0      # 0 = auto-scale to fit trace widths
output_format          = "3mf"    # "stl", "3mf", or "both"
output_dir             = "./output"
```

Settings are merged in order: built-in defaults → TOML file → CLI arguments.

---

## MCP server (Claude Desktop)

kicad2print exposes a full KiCad toolset as an MCP server for use with Claude Desktop.

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

### Available tools

| Tool | Description |
|---|---|
| `scan_project` | **Start here** — scans a folder, returns file list, BOM, and rendered board image |
| `read_kicad_file` | Read any `.kicad_pcb` or `.kicad_sch` file |
| `write_kicad_file` | Write a modified design file back to disk |
| `render_pcb` | Render the board using KiCad's raytracer (top/bottom/side views) |
| `run_drc` | Run Design Rules Check — returns JSON report of violations |
| `export_netlist` | Export schematic netlist — full component + net connectivity |
| `export_bom` | Export Bill of Materials as CSV |
| `list_footprint_libraries` | List all installed `.pretty` footprint libraries |
| `list_footprints_in_library` | List all footprints in a library |
| `get_footprint` | Get the full `.kicad_mod` S-expression for a footprint |
| `search_footprint` | Search footprints by name across all libraries |
| `export_layer_svg` | Export PCB layers as SVG for routing inspection |
| `convert_pcb` | Convert to 3D-printable substrate (STL/3MF) with preview |

### Example session

```
You:   Scan my project at /home/me/projects/myboard/kicad
Claude: [renders the board, shows BOM, lists all files]

You:   I swapped the Arduino Uno for a Nano — update the PCB
Claude: [searches footprint libraries, reads PCB, modifies footprint,
         writes back, renders updated board, runs DRC]
```

> **Note:** `render_pcb` and the KiCad CLI tools require `kicad-cli` to be installed (part of KiCad 9+). Footprint search requires the `kicad-library` package (`sudo pacman -S kicad-library` on Arch/Manjaro).

---

## Output files

Each run produces in `--output-dir` (default `./output/`):

| File | Description |
|---|---|
| `boardname.stl` | Binary STL for slicers (when format = stl or both) |
| `boardname.3mf` | 3MF with metadata (when format = 3mf or both) |
| `boardname_preview.html` | Self-contained interactive 3D viewer (three.js) |

---

## How it works

```
.kicad_pcb
    │
    ├─ parser/sexp.rs    Tokenize S-expressions → SexpNode tree
    ├─ parser/kicad.rs   Walk tree → PcbData (traces, vias, pads, outline)
    ├─ autoscale.rs      Compute scale factor to fit traces in channels
    ├─ geometry/         Tessellate 3D substrate mesh (z-buffer triangle mesh)
    ├─ export/stl.rs     Write binary STL
    ├─ export/threemf.rs Write 3MF (ZIP + XML)
    ├─ export/html.rs    Write interactive three.js preview
    └─ render.rs         Software rasterizer → PNG (for MCP image responses)
```

**Coordinate convention:** KiCad uses Y-down; kicad2print negates Y at parse time so all geometry code uses standard Y-up coordinates.

**Scaling:** if any trace is narrower than `channel_width_mm`, the entire board is scaled up uniformly so the narrowest trace exactly fills a channel. This preserves relative component spacing.

---

## Building & development

```bash
cargo build           # debug
cargo build --release # optimised
cargo test            # unit tests
cargo clippy          # lints
cargo fmt             # format
cargo doc --open      # API docs
```

---

## License

AGPL-3.0 — see [LICENSE](LICENSE).
