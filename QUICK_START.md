# Quick Start Guide

## Installation & First Run

### 1. Build the project

```bash
cd /home/rsalvador/kicad2print
cargo build --release
```

The binary will be at: `target/release/kicad2print`

### 2. Test it (without a real KiCad file)

```bash
./target/release/kicad2print --help
```

Output:
```
Transform a KiCad .kicad_pcb file into a 3D-printable substrate
for hybrid PCB construction using wire traces and copper eyelets.

USAGE:
    kicad2print [OPTIONS] <INPUT>

ARGS:
    <INPUT>    Path to the KiCad PCB file (.kicad_pcb)

OPTIONS:
    -c, --config <FILE>              Path to TOML config file
    --channel-width <MM>             Override channel_width_mm
    ...
```

### 3. Use with your KiCad board

```bash
./target/release/kicad2print my_board.kicad_pcb --verbose
```

You should see output like:
```
📋 Loading configuration...
📖 Parsing KiCad file: my_board.kicad_pcb
=== PCB Design Summary ===
Board size: 50.00mm × 30.00mm
Front copper traces: 12
Back copper traces: 8
Vias: 5
Pads with drills: 24
...
```

### 4. Create a config file for your project

Copy the example:
```bash
cp default_config.toml my_board_config.toml
```

Edit `my_board_config.toml` to customize:
```toml
channel_width_mm = 0.9    # Adjust for your wire size
eyelet_style = "hole"     # Change to "indent" if you don't have a drill press
eyelet_diameter_mm = 1.3  # Match your copper eyelets
```

Use it:
```bash
./target/release/kicad2print my_board.kicad_pcb --config my_board_config.toml
```

## What Works Right Now

✅ Parse KiCad PCB files (S-expression format)
✅ Extract traces, vias, pads, and board outlines  
✅ Auto-scale boards to fit narrow traces
✅ Load configuration from TOML files
✅ Override settings via CLI arguments
✅ Pretty-print design summaries
✅ Structured error messages

## What's Next

The geometry generation and export is planned but not yet implemented:
- 3D substrate modeling (solid block with channels)
- STL/3MF file generation
- Use of truck-* crates for CSG operations

## Debugging

### Verbose mode

```bash
./target/release/kicad2print board.kicad_pcb --verbose
```

Shows detailed information at each pipeline stage.

### Check configuration

```bash
./target/release/kicad2print board.kicad_pcb \
  --channel-width 1.5 \
  --eyelet-style hole \
  --verbose
```

The printed config will show what's actually being used.

### Parse only (test the parser)

Currently, the tool parses but doesn't generate geometry. To verify parsing works:

```bash
./target/release/kicad2print board.kicad_pcb --verbose 2>&1 | grep -A 20 "PCB Design Summary"
```

This shows how many traces, vias, and pads were successfully extracted.

## Development

### Run tests

```bash
cargo test
```

### Format code

```bash
cargo fmt
```

### Check for issues

```bash
cargo clippy
```

### Build documentation

```bash
cargo doc --open
```

Opens HTML documentation of all public APIs.

## File Locations

After first run, check these:
- Binary: `target/release/kicad2print`
- Source: `src/` directory
- Config: `*.toml` files in project directory
- Output: `output/` directory (will be created by geometry/export stages)

## Examples

### Minimal usage

```bash
./target/release/kicad2print board.kicad_pcb
```

### Full configuration override

```bash
./target/release/kicad2print board.kicad_pcb \
  --config custom.toml \
  --channel-width 1.0 \
  --eyelet-style indent \
  --substrate-thickness 2.5 \
  --scale 1.5 \
  --format stl \
  --output-dir ./models \
  --verbose
```

### Using different board files in sequence

```bash
./target/release/kicad2print board1.kicad_pcb --verbose
./target/release/kicad2print board2.kicad_pcb --verbose
./target/release/kicad2print board3.kicad_pcb --verbose
```

## Troubleshooting

**Error: "Failed to read KiCad file"**
- Check the file path is correct
- Ensure the file is a valid .kicad_pcb file

**Error: "Failed to parse S-expressions"**
- The file might be corrupted or from an unsupported KiCad version
- Try exporting from KiCad again

**Warning: "No board outline found"**
- This is okay—the tool will use trace bounding box as fallback
- Check that your KiCad design has Edge.Cuts layer defined

**No output files generated**
- The tool currently only parses (geometry/export TODO)
- Check console output to verify parsing succeeded

## Next Steps

1. **Test with your KiCad projects** - verify parsing works
2. **Understand the code** - read LEARNING.md for Rust concepts
3. **Contribute geometry/export** - implement 3D generation
4. **Extend with new features** - see ARCHITECTURE.md for how to add features

## Getting Help

- **Rust concepts**: See `LEARNING.md`
- **Code architecture**: See `ARCHITECTURE.md`
- **README**: General features and usage
- **Source code**: Every public item has doc comments
- **Rust Book**: https://doc.rust-lang.org/book/

