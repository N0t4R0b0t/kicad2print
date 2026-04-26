# kicad2print

Convert KiCad PCB designs into 3D-printable models for hybrid PCB construction.

This tool generates STL/3MF 3D models from KiCad `.kicad_pcb` files, enabling you to 3D-print custom PCB substrates with integrated trace channels and via guide marks. Perfect for the "hybrid PCB" construction method using FDM 3D printing, copper wire traces, and copper eyelets.

## Features

- 📝 Parses KiCad S-expression `.kicad_pcb` files
- 🧊 Generates 3D-printable substrate models (STL/3MF)
- 🌐 Interactive 3D preview with three.js (rotate, pan, zoom)
- ⚙️ Configurable via TOML + CLI arguments
- 📊 Extracts traces, vias, pads, and board outlines with exact positioning
- 🎯 Supports both "hole" (through-hole) and "indent" (guide dimple) via styles
- ✅ Preserves component hole spacing at 1:1 scale — components fit perfectly
- ⚠️ Geometry validation: warns if channel depth or eyelet indents too shallow

## Quick Start

### Installation

1. **Install Rust** (if you don't have it):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Build kicad2print**:
   ```bash
   cd kicad2print
   cargo build --release
   ```

3. **Run it**:
   ```bash
   ./target/release/kicad2print your_board.kicad_pcb
   ```

### Basic Usage

```bash
# Using defaults (outputs to ./output/)
kicad2print my_board.kicad_pcb

# With a config file
kicad2print my_board.kicad_pcb --config my_settings.toml

# Overriding specific settings
kicad2print my_board.kicad_pcb --channel-width 0.8 --eyelet-style hole

# Verbose output for debugging
kicad2print my_board.kicad_pcb --verbose

# Generate both STL and 3MF
kicad2print my_board.kicad_pcb --format both

# Auto-open in 3D viewer after conversion
kicad2print my_board.kicad_pcb --view
```

## Configuration

Settings are loaded in this order (later overrides earlier):
1. **Defaults** (built-in)
2. **TOML config file** (default: `kicad2print.toml`)
3. **CLI arguments** (override everything)

### Configuration File (`kicad2print.toml`)

```toml
# Width of the groove channels that will hold copper wire traces
channel_width_mm = 1.2

# Depth of channels below the substrate surface
channel_depth_mm = 0.5

# Via representation: "hole" or "indent"
# "hole": full through-holes for drilling after printing
# "indent": shallow dimples for guiding copper eyelets
eyelet_style = "indent"

# Diameter of via holes or indent dimples
eyelet_diameter_mm = 1.5

# Depth of indent dimples (only used when eyelet_style = "indent")
indent_depth_mm = 0.3

# Diameter of component pad through-holes
pad_hole_diameter_mm = 0.8

# Total thickness of the printed substrate
substrate_thickness_mm = 3.0

# Manual scale factor (0 = true size, preserves component spacing)
# Only use if you want a deliberately scaled mock-up
scale_factor = 0.0

# Output format: "stl", "3mf", or "both"
output_format = "stl"

# Directory for output files
output_dir = "./output"
```

### Command-Line Arguments

```
OPTIONS:
  -c, --config <FILE>              TOML config file path
  --channel-width <MM>             Trace channel width
  --channel-depth <MM>             Trace channel depth
  --eyelet-style <STYLE>           "hole" or "indent"
  --eyelet-diameter <MM>           Via diameter
  --indent-depth <MM>              Indent depth
  --pad-hole-diameter <MM>         Pad hole diameter
  --substrate-thickness <MM>       Board thickness
  --scale <FACTOR>                 Manual scale factor (0 = auto)
  --format <FORMAT>                "stl", "3mf", or "both"
  --output-dir <DIR>               Output directory
  --view                           Auto-open result in 3D viewer
  -v, --verbose                    Detailed output during processing
  -h, --help                       Show help
  -V, --version                    Show version
```

## Project Structure & Code Walkthrough

### Directory Layout

```
kicad2print/
├── Cargo.toml              # Rust project manifest with dependencies
├── Cargo.lock              # Locked dependency versions
├── README.md               # This file
└── src/
    ├── main.rs             # CLI entry point and orchestration
    ├── config.rs           # Configuration loading and merging
    ├── pcb.rs              # Core PCB data structures
    ├── autoscale.rs        # Auto-scaling logic
    └── parser/
        ├── mod.rs          # Parser module entry point
        ├── sexp.rs         # S-expression tokenizer and parser
        └── kicad.rs        # KiCad file walker and extractor
```

### Module Deep-Dive

#### 1. **pcb.rs** - PCB Data Model

This module defines all the fundamental data structures that represent a PCB design.

```rust
// A 2D point in millimeters
pub struct Point2 { pub x: f64, pub y: f64 }

// A straight trace segment on a copper layer
pub struct Trace {
    pub layer: CopperLayer,  // FCu (front) or BCu (back)
    pub start: Point2,
    pub end: Point2,
    pub width: f64,          // In mm
}

// A via (vertical interconnect) connecting layers
pub struct Via {
    pub center: Point2,
    pub drill: f64,
}

// Component pad with through-hole
pub struct Pad {
    pub center: Point2,
    pub drill: f64,
}

// Board outline from Edge.Cuts layer
pub struct BoardOutline {
    pub vertices: Vec<Point2>,
    pub bbox: BoundingBox,
}

// Complete PCB design data
pub struct PcbData {
    pub outline: Option<BoardOutline>,
    pub traces_fcu: Vec<Trace>,
    pub traces_bcu: Vec<Trace>,
    pub arc_traces: Vec<ArcTrace>,
    pub vias: Vec<Via>,
    pub pads: Vec<Pad>,
}
```

**Key Design Decision**: All Y-coordinates are negated during parsing to convert from KiCad's Y-down convention to standard mathematical Y-up convention. This way, all code that uses `Point2` works with standard coordinates.

#### 2. **parser/sexp.rs** - S-Expression Parser

KiCad uses Lisp-like S-expressions to store design data. This module parses that format.

**The Process:**
1. **Tokenizer** - Reads the file character-by-character, producing tokens:
   - `LParen` `(`
   - `RParen` `)`
   - `Atom` (quoted strings or unquoted identifiers)

   ```rust
   let tokenizer = Tokenizer::new(input_string);
   let tokens = tokenizer.tokenize()?;
   ```

2. **Parser** - Builds a tree of `SexpNode` from tokens:
   ```rust
   enum SexpNode {
       Atom(String),
       List(Vec<SexpNode>),
   }
   ```

3. **Helper Methods** on `SexpNode`:
   ```rust
   node.as_atom()           // Get atom string
   node.as_list()           // Get child list
   node.get_child("name")   // Find first child with matching atom
   node.nth(index)          // Get nth child
   ```

**Example**: Parsing `(segment (start 10.5 20.3) (end 30.0 40.1) (width 0.25) (layer "F.Cu"))`
- Root is a `List` with atoms: `"segment"`, and child lists: `(start ...)`, `(end ...)`, etc.
- To get the start point: `node.get_child("start")` returns the `(start 10.5 20.3)` node
- Then use `get_xy_point()` helper to extract the X and Y values

#### 3. **parser/kicad.rs** - KiCad-Specific Extractor

This module walks the S-expression tree and extracts meaningful PCB elements.

**Key Functions:**

```rust
/// Main entry point - walks the entire parsed tree
pub fn walk_kicad_tree(nodes: &[SexpNode]) -> Result<PcbData>

/// Extracts traces from (segment ...) nodes
fn parse_segment(node: &SexpNode) -> Result<Trace>

/// Extracts vias from (via ...) nodes
fn parse_via(node: &SexpNode) -> Result<Via>

/// Extracts component pads from (pad ...) nodes within (footprint ...)
fn parse_footprint_pads(node: &SexpNode) -> Result<Vec<Pad>>

/// Chains unordered outline segments into a closed polygon
fn chain_outline_segments(segments: Vec<(Point2, Point2)>) -> Result<BoardOutline>
```

**Coordinate Transform:**
The key line in `get_xy_point()`:
```rust
// Negate Y to convert from KiCad's Y-down to standard Y-up
Some(Point2::new(x, -y))
```

#### 4. **config.rs** - Configuration Management

Handles loading settings from TOML files and merging CLI arguments.

**Enums:**
```rust
pub enum EyeletStyle { Hole, Indent }
pub enum OutputFormat { Stl, ThreeM, Both }
```

**The Config Struct:**
```rust
pub struct Config {
    pub channel_width_mm: f64,        // Default: 1.2
    pub channel_depth_mm: f64,        // Default: 0.5
    pub eyelet_style: EyeletStyle,    // Default: Indent
    pub eyelet_diameter_mm: f64,      // Default: 1.5
    // ... etc
}

impl Config {
    // Load from TOML file (returns defaults if file doesn't exist)
    pub fn from_file(path: &Path) -> Result<Self>

    // Merge CLI overrides into this config
    pub fn merge_cli_overrides(&mut self, overrides: &CliOverrides)
}
```

**Pattern**: The `CliOverrides` struct mirrors `Config` but uses `Option<T>` for each field. A `None` value means "don't override"; `Some(value)` means "use this value".

#### 5. **autoscale.rs** - Auto-Scaling Logic

Computes the minimum scale factor needed to fit narrow traces into configured channels.

```rust
pub fn compute_scale_factor(pcb: &PcbData, config: &Config) -> f64 {
    // If user specified scale_factor > 0, use that
    if config.scale_factor > 0.0 {
        return config.scale_factor;
    }

    // Find narrowest trace in design
    let min_trace_width = /* minimum of all trace widths */;

    // If traces are narrower than desired channel width, scale up
    // E.g., if trace is 0.1mm and channel should be 1.2mm:
    // scale = 1.2 / 0.1 = 12.0x
    if min_trace_width < config.channel_width_mm {
        config.channel_width_mm / min_trace_width
    } else {
        1.0
    }
}
```

#### 6. **main.rs** - CLI Entry Point and Orchestration

The main function coordinates the entire pipeline:

```rust
fn main() -> Result<()> {
    // 1. Parse CLI arguments (using clap)
    let args = Args::parse();

    // 2. Load configuration (TOML file + CLI overrides)
    let mut config = Config::from_file(config_path)?;
    config.merge_cli_overrides(&args.to_overrides()?);

    // 3. Parse the KiCad file
    let pcb_data = parser::parse_pcb(&args.input)?;

    // 4. Calculate scale factor
    let scale_factor = autoscale::compute_scale_factor(&pcb_data, &config);

    // 5. Apply scaling
    let pcb_data = pcb_data.scale(scale_factor);

    // 6-7. Geometry generation and export (TODO)
    //...

    Ok(())
}
```

**Error Handling Pattern**:
Uses `anyhow::Result<T>` throughout. Context is added at each layer:
```rust
let pcb_data = parser::parse_pcb(&args.input)
    .context("Failed to parse KiCad file")?;
```

This way, errors bubble up with clear context about what failed.

## Data Flow

```
KiCad File (kicad_pcb)
    ↓
[parser/sexp.rs] Tokenize → Parse S-expressions
    ↓
[parser/kicad.rs] Walk tree → Extract design elements
    ↓
[pcb.rs] PcbData (traces, vias, pads, outline)
    ↓
[autoscale.rs] Calculate scale factor
    ↓
Apply scaling to PcbData
    ↓
[geometry/*] Generate 3D model (TODO)
    ↓
[export/*] Write STL/3MF files (TODO)
```

## Learning Resources

If you're learning Rust, here are the key concepts used in this project:

1. **Enums** (`CopperLayer`, `SexpNode`, `EyeletStyle`) - Type-safe alternatives to strings/integers
2. **Result<T, E>** - Error handling without exceptions
3. **Option<T>** - Nullable values without nulls
4. **Trait Objects** - Generic behavior (`AsRef<Path>`)
5. **Pattern Matching** - `match` expressions for exhaustive handling
6. **Derive Macros** - `#[derive(Debug, Clone)]` generates implementations
7. **Module System** - Organizing code into logical modules
8. **CLI Parsing** - Using `clap` crate with derive macros
9. **Configuration Management** - Loading from TOML with `serde`
10. **Error Context** - Using `anyhow` for rich error messages

## Current Status

✅ **Implemented:**
- KiCad S-expression parsing
- PCB design data extraction
- Configuration management
- CLI argument parsing
- Auto-scaling logic

⏳ **TODO:**
- 3D geometry generation (substrate, channels, eyelets, pads)
- STL file export
- 3MF file export
- Comprehensive testing
- Documentation of the geometry module

## Building & Testing

```bash
# Build in debug mode
cargo build

# Build optimized release
cargo build --release

# Run tests
cargo test

# Check for compilation errors
cargo check

# Format code
cargo fmt

# Lint
cargo clippy
```

## Contributing

This is a learning project! Comments and documentation are intentionally verbose to help others understand the code.

## License

MIT or Apache 2.0 (dual license)

## References

- [KiCad File Formats](https://dev-docs.kicad.org/en/file-formats/)
- [Rust Book](https://doc.rust-lang.org/book/)
- [Clap - CLI Parser](https://docs.rs/clap/latest/clap/)
- [Serde - Serialization](https://serde.rs/)
