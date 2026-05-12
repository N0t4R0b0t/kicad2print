# Architecture

## Data flow

```
.kicad_pcb (S-expression text)
    │
    ├─ parser/sexp.rs      Tokenize → SexpNode tree
    ├─ parser/kicad.rs     Walk tree → PcbData
    │                       (traces, arc traces, vias, pads, footprints, outline)
    ├─ autoscale.rs        Compute uniform scale so narrowest trace ≥ channel_width
    ├─ geometry/mod.rs     Tessellate substrate mesh (Triangle3D list)
    ├─ export/stl.rs       Binary STL
    ├─ export/threemf.rs   3MF (ZIP + XML)
    ├─ export/html.rs      Self-contained three.js HTML preview
    └─ render.rs           Software z-buffer rasterizer → PNG (MCP image responses)
```

## Module map

```
src/
├── main.rs            CLI entry point; dispatches to MCP server or CLI pipeline
├── mcp.rs             MCP server (rmcp 1.5) — all KiCad tools for Claude Desktop
├── config.rs          Config struct, TOML loading, CLI override merging
├── pcb.rs             PcbData, Trace, Via, Pad, Footprint, BoardOutline
├── autoscale.rs       Scale factor computation
├── render.rs          Software rasterizer for MCP PNG previews
├── geometry/
│   └── mod.rs         3D mesh generation (substrate slab, channels, holes)
├── export/
│   ├── mod.rs         Orchestration — calls stl/threemf/html writers
│   ├── stl.rs         Binary STL writer
│   ├── threemf.rs     3MF writer (ZIP + XML)
│   └── html.rs        three.js HTML preview generator
└── parser/
    ├── mod.rs         Public parse_pcb() entry point
    ├── sexp.rs        S-expression tokenizer + parser → SexpNode
    └── kicad.rs       KiCad tree walker → PcbData extractor
```

## Key design decisions

### Coordinate system
KiCad uses Y-down. kicad2print negates Y at parse time (`get_xy_point` in `kicad.rs`) so all downstream code — geometry, export, render — works in standard Y-up. Rotation angles are converted from KiCad's CCW-positive-in-Y-down to CCW-positive-in-Y-up.

### Mesh representation
`Mesh3D` is a flat list of `Triangle3D { normal, vertices }` with no shared vertices. This is intentional: STL and the three.js preview both want unindexed triangles, and it simplifies geometry generation (no index bookkeeping). The 3MF exporter re-indexes for compactness.

### Scaling
If any trace width is narrower than `channel_width_mm`, the entire board scales up uniformly: `scale = channel_width / min_trace_width`. This keeps component hole spacing correct so drilled holes still fit parts, just on a larger board.

### MCP server
`mcp.rs` uses the `rmcp` crate (v1.5, `#[tool_router]` macro pattern). Each tool is an `async fn` on `KiCadServer`. Tools that invoke `kicad-cli` spawn it via `tokio::process::Command` and capture stdout/stderr. The server runs over stdio (stdin/stdout), which is the standard Claude Desktop transport.

Two modes share the same binary, selected by the `--mcp` flag:
```
kicad2print [args]      CLI mode — convert a PCB
kicad2print --mcp       MCP server mode — serve tools over stdio
```

## MCP tool categories

| Category | Tools | Mechanism |
|---|---|---|
| File I/O | `read_kicad_file`, `write_kicad_file` | `tokio::fs` |
| KiCad CLI | `render_pcb`, `run_drc`, `export_netlist`, `export_bom`, `export_layer_svg` | `kicad-cli` subprocess |
| Footprint library | `list_footprint_libraries`, `list_footprints_in_library`, `get_footprint`, `search_footprint` | `tokio::fs` walk of `.pretty` dirs |
| Project entry | `scan_project` | Combines file walk + BOM export + render |
| Substrate | `convert_pcb` | Full kicad2print pipeline |

## Adding a new tool

1. Add a params struct deriving `Serialize, Deserialize, schemars::JsonSchema`
2. Add an `async fn` with `#[tool(description = "...")]` inside the `#[tool_router] impl KiCadServer` block
3. Return `Result<CallToolResult, McpError>` using `Content::text(...)` or `Content::image(b64, "image/png")`

## Dependency rationale

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `serde` + `toml` | Config file loading |
| `nom` | S-expression parser (custom, not nom combinators — nom pulled in transitively) |
| `geo` | 2D polygon boolean ops for board outline triangulation |
| `earcutr` | Earcut triangulation for polygons |
| `nalgebra` | 3D vector math in geometry module |
| `zip` + `quick-xml` | 3MF format (ZIP container with XML content) |
| `image` (png only) | PNG encoding for MCP preview images |
| `base64` | Encode PNG bytes for MCP image content blocks |
| `rmcp` | MCP server SDK (stdio transport, `#[tool_router]` macro) |
| `tokio` | Async runtime for MCP server and kicad-cli subprocess calls |
| `schemars` | JSON Schema generation for MCP tool parameter descriptions |
| `anyhow` / `thiserror` | Error handling with context |
