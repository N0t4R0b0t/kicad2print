# Quick Start

## CLI — convert a KiCad board to a 3D-printable substrate

```bash
# 1. Build (or download a release binary)
cargo build --release

# 2. Convert a board
./target/release/kicad2print my_board.kicad_pcb --verbose

# 3. Open the interactive HTML preview
./target/release/kicad2print my_board.kicad_pcb --view
```

Output files appear in `./output/`:
- `boardname.3mf` — import into your slicer
- `boardname_preview.html` — open in a browser for 3D inspection

## MCP server — work on KiCad projects in Claude Desktop

### 1. Add to `claude_desktop_config.json`

```json
{
  "mcpServers": {
    "kicad2print": {
      "command": "/path/to/kicad2print",
      "args": ["--mcp"]
    }
  }
}
```

Restart Claude Desktop.

### 2. Start a session with `scan_project`

Tell Claude:
```
Scan my KiCad project at /home/me/projects/myboard
```

Claude will discover all PCB and schematic files, export the BOM, and render the board so it has full visual context before you ask anything.

### 3. Typical tasks

**Preview the board:**
```
Render the top and bottom of the board
```

**Check for problems:**
```
Run DRC and fix any violations
```

**Swap a component:**
```
Replace the Arduino Uno footprint with an Arduino Nano.
Find the right footprint in the library.
```

**Update routing after moving a part:**
```
U1 moved 5mm to the right. Update the traces and re-render.
```

**Convert to 3D-printable substrate:**
```
Convert the board to a 3MF substrate with 0.8mm channels
```

## Configuration file

Copy `default_config.toml` to your project:

```bash
cp default_config.toml kicad2print.toml
```

Key settings to tune for your build:

```toml
channel_width_mm  = 0.9   # Match your wire gauge (0.6–1.2 typical)
eyelet_style      = "hole"   # "hole" if you have a drill press, else "indent"
eyelet_diameter_mm = 1.3   # Match your copper eyelets
substrate_thickness_mm = 2.5
```

## Troubleshooting

| Error | Cause | Fix |
|---|---|---|
| `Failed to read KiCad file` | Wrong path | Check the path |
| `No board outline found` | Missing Edge.Cuts layer | Add an outline in KiCad |
| `render_pcb` fails | `kicad-cli` not found | Install KiCad 9+ |
| `search_footprint` returns nothing | Library not installed | `sudo pacman -S kicad-library` |
