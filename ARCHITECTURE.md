# kicad2print Architecture

## Project Overview

`kicad2print` is a Rust CLI tool that transforms KiCad PCB designs into 3D-printable models. It's specifically designed for the "hybrid PCB" construction method: 3D-printed substrates with wire traces and copper eyelets.

## High-Level Data Flow

```
┌─────────────────┐
│  .kicad_pcb     │  KiCad design file (S-expressions)
│  file (text)    │
└────────┬────────┘
         │
         ▼
┌─────────────────────────┐
│ parser/sexp.rs          │  Tokenize & parse S-expressions
│ - Tokenizer             │  → SexpNode tree
│ - Parser                │
└────────┬────────────────┘
         │
         ▼
┌─────────────────────────┐
│ parser/kicad.rs         │  Walk SexpNode tree
│ - walk_kicad_tree()     │  → Extract design elements
│ - parse_segment()       │  → PcbData struct
│ - parse_via()           │
│ - parse_footprint_pads()│
└────────┬────────────────┘
         │
         ▼
┌─────────────────────────┐
│ autoscale.rs            │  Calculate scale factor
│ - compute_scale_factor()│  to fit traces in channels
└────────┬────────────────┘
         │
         ▼
┌─────────────────────────┐
│ geometry/ (TODO)        │  Generate 3D solid model
│ - substrate.rs          │  - Base rectangular block
│ - channels.rs           │  - Channel cutouts
│ - eyelets.rs            │  - Via holes/indents
│ - pads.rs               │  - Pad through-holes
└────────┬────────────────┘
         │
         ▼
┌─────────────────────────┐
│ export/ (TODO)          │  Write output files
│ - stl.rs                │  - Binary STL format
│ - threemf.rs            │  - 3MF format
└────────┬────────────────┘
         │
         ▼
┌─────────────────┐
│ Output files    │  Ready for 3D printing
│ (STL/3MF)       │  in slicer software
└─────────────────┘
```

## Module Dependency Graph

```
main.rs (orchestration)
  ├─ config.rs (settings)
  ├─ parser/mod.rs (file reading)
  │   ├─ parser/sexp.rs (tokenizing & parsing)
  │   └─ parser/kicad.rs (extracting design elements)
  │       └─ pcb.rs (data types)
  ├─ autoscale.rs (compute scale)
  │   └─ pcb.rs
  └─ (future) geometry/mod.rs → export/mod.rs
```

## File Organization

```
src/
├── main.rs (180 lines)
│   └─ CLI entry point with clap
│   └─ Config loading and merging
│   └─ Pipeline orchestration
│
├── config.rs (280 lines)
│   └─ Configuration structs with serde
│   └─ Enum types for settings
│   └─ TOML loading and CLI merging
│
├── pcb.rs (400 lines)
│   └─ Core PCB data types
│   └─ Helper methods (scale, translate, distance)
│   └─ Pretty-printing for debugging
│
├── autoscale.rs (70 lines)
│   └─ Auto-scaling logic
│   └─ Trace width analysis
│   └─ Unit tests
│
└── parser/
    ├── mod.rs (20 lines)
    │   └─ Public parse_pcb() entry point
    │
    ├── sexp.rs (300 lines)
    │   ├─ SexpNode enum (Atom, List)
    │   ├─ Tokenizer (char-by-char scanning)
    │   ├─ Parser (token-to-tree conversion)
    │   └─ Helper methods on SexpNode
    │
    └── kicad.rs (400 lines)
        ├─ walk_kicad_tree() - main walker
        ├─ parse_segment() - traces
        ├─ parse_arc() - arc traces
        ├─ parse_via() - vias
        ├─ parse_footprint_pads() - components
        ├─ parse_gr_* functions - board outline
        ├─ chain_outline_segments() - polygon assembly
        └─ Helper functions (get_xy_point, get_string_value)

Total: ~1600 lines of well-documented code
```

## Key Design Decisions

### 1. **Coordinate System Transformation**
- **Problem**: KiCad uses Y-down convention (Y increases downward on screen)
- **Solution**: Negate all Y-coordinates at parse time
- **Benefit**: All downstream code works with standard math convention (Y-up)

### 2. **S-Expression as Intermediate Representation**
- **Problem**: Direct parsing of KiCad's complex S-expression format is error-prone
- **Solution**: Two-stage pipeline: tokenize → parse tree → extract data
- **Benefit**: Clear separation of concerns; easier to debug and test each stage

### 3. **Enum-Based Configuration**
- **Problem**: Magic strings like `"hole"` and `"indent"` are error-prone
- **Solution**: Use Rust enums with derive for parsing and serialization
- **Benefit**: Type-safe; impossible to use invalid values; compiler checks exhaustiveness

### 4. **Option & Result Instead of Null/Exceptions**
- **Problem**: Missing data or errors can be silently ignored
- **Solution**: Required `Option<T>` and `Result<T, E>` handling
- **Benefit**: Compiler forces error handling; no silent failures

### 5. **Modular Pipeline**
- **Problem**: Monolithic parsing and processing is hard to test
- **Solution**: Each stage (parse → scale → generate → export) is a separate function
- **Benefit**: Can test and debug each stage independently

## Testing Strategy

The project uses `#[cfg(test)]` modules for unit testing:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_scale_factor() {
        // Test logic for scale calculation
    }
}

// Run with: cargo test
```

## Error Handling Philosophy

- Use `anyhow::Result<T>` for general errors with rich context
- Use custom error enums with `thiserror` only when callers need to distinguish error types
- Always add `.context("description")` when propagating errors
- Panics reserved for programmer errors, not user input

## Future Additions

### Phase 2: Geometry Generation
Implement the `geometry/` module:
- ✅ Plan complete (in PLAN.md)
- ⏳ Requires truck-* crates for CSG operations
- Implementation sequence: substrate → channels → eyelets → pads → boolean subtraction

### Phase 3: Export
Implement the `export/` module:
- ✅ Plan complete
- ⏳ STL: uses truck-polymesh for mesh tessellation
- ⏳ 3MF: manually generate ZIP + XML using zip and quick-xml crates

### Phase 4: Advanced Features
- Board visualization (SVG output)
- Wire length calculation
- Material estimate (weight, filament)
- Cost calculation
- Design validation (trace spacing, pad diameter)

## Performance Considerations

- **S-expression parsing**: O(n) where n = file size; typically <100ms for 10MB files
- **Geometry generation**: O(m) where m = number of design elements; could be slow for >1000 features (use chunking/parallelism)
- **Export**: O(k) where k = number of triangles; tessellation tolerance affects size

## Dependencies

Core crates used:
- `clap`: 4.5 - CLI argument parsing
- `serde` + `toml`: Configuration management
- `nom`: 7.1 - Parser combinators (for potential future use)
- `anyhow`: Error handling with context
- `nalgebra`: 0.33 - 2D/3D math (unused currently, but prepared for geometry)

Not yet integrated:
- `truck-modeling`: 3D solid modeling
- `truck-shapeops`: Boolean operations (CSG)
- `truck-meshing`: Mesh generation
- `truck-polymesh`: STL export
- `zip` + `quick-xml`: 3MF export

## Code Quality

- **Documentation**: Every public item has doc comments with examples
- **Testing**: Unit tests for critical functions (autoscale, parsing edge cases)
- **Warnings**: Clean build with only unused code warnings (from future code)
- **Linting**: Follows Rust API guidelines and clippy recommendations

## Learning Value

This project is designed as a learning resource:
- Well-commented code (see comments throughout)
- Progressive complexity (parser → data structures → orchestration)
- Multiple Rust concepts demonstrated (enums, Result, generics, traits, modules)
- See `LEARNING.md` for detailed explanations of Rust concepts used

## How to Extend

To add a new feature (e.g., support for inner copper layers):

1. **Update `pcb.rs`**: Add new data structure
2. **Update `parser/kicad.rs`**: Add extraction logic
3. **Add tests**: Verify with real board files
4. **Update `config.rs`**: Add configuration option if needed
5. **Update `main.rs`**: Wire it into the pipeline

Example: To support inner copper layers:
```rust
// In pcb.rs:
pub struct PcbData {
    // ... existing ...
    pub traces_inl1: Vec<Trace>,  // Inner layer 1
    pub traces_inl2: Vec<Trace>,  // Inner layer 2
}

// In parser/kicad.rs:
"In1.Cu" => CopperLayer::In1,
"In2.Cu" => CopperLayer::In2,

// In geometry/channels.rs:
// Add channel generation for inner layers
```

## References

- KiCad S-expression format: https://dev-docs.kicad.org/en/file-formats/
- Rust Book: https://doc.rust-lang.org/book/
- API Guidelines: https://rust-lang.github.io/api-guidelines/
