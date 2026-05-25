# KiCad MCP Tools

> **Read this before using these tools on a board you care about.**

The kicad2print MCP server exposes a set of tools that let an AI model read, query, and modify KiCad project files — PCBs and schematics — through a chat interface. This document explains what those tools do, what they genuinely help with, and where they will actively make things worse.

---

## The honest disclaimer

These tools operate on KiCad S-expression files as structured text. They have no understanding of your design intent, electrical requirements, signal integrity, mechanical constraints, or manufacturing rules beyond what can be derived from a flat file.

**Things an AI using these tools cannot do:**

- Understand *why* a net is routed a particular way
- Recognise when a mechanical constraint overrides an electrical one
- Know that a trace is intentionally long, curved, or wide for a reason
- Account for component clearances that aren't in the design rules
- Verify that a footprint physically matches a real component
- See the schematic context behind a PCB change (or vice versa)
- Detect when a "valid" file will produce a broken board

**The real risk is confident incorrectness.** An AI can produce a syntactically valid `.kicad_pcb` that passes DRC but routes a trace through a keep-out zone, assigns the wrong net, bridges two power rails, or removes traces it misidentified as dangling — all without any warning. The file will open. KiCad will not complain. The board will be wrong.

If you find yourself in a session where things are getting worse with each tool call, stop. Open KiCad. Undo manually. The tools are not a substitute for the application.

---

## Where they genuinely help

The tools are most useful for **inspection and targeted single-step edits** where the outcome is immediately verifiable:

- Checking net names before routing — `list_nets` tells you the exact string KiCad uses so you don't assign the wrong net to a trace
- Verifying what's physically at a coordinate before touching it — `query_pads_in_region` and `check_trace_clearance` expose collisions before they're committed
- Confirming a specific pad's net without opening the file — `get_net_for_pad`
- Checking if a recently added trace actually connects two pads — `verify_connectivity`
- Adding a power symbol correctly (lib definition + placed instance, atomically) — `add_power_symbol`
- Running DRC and seeing the board render alongside the violations
- Swapping a footprint, reading the BOM, or converting to a substrate model

They are least useful — and most dangerous — for large routing sessions, net renaming across many traces, or any change where you can't immediately verify the result in KiCad.

---

## Tool reference

### Inspection tools (read-only, safe to call freely)

| Tool | What it does |
|---|---|
| `scan_project` | Entry point — renders board, returns BOM and all project files |
| `render_pcb` | Render top/bottom/side 3D view of a `.kicad_pcb` |
| `render_schematic` | Render a `.kicad_sch` schematic as a PNG |
| `list_nets` | **All nets with their connected pads.** Call this first, before any edit, to discover correct net names. Never guess. |
| `get_net_for_pad` | Net name, absolute position, and size of one pad by reference + number |
| `query_pads_in_region` | All pads whose centre falls inside a bounding box — use before routing |
| `check_trace_clearance` | Collision and clearance check for a proposed segment — call before `add_trace` |
| `verify_connectivity` | BFS through traces and vias to confirm two pads are physically wired |
| `export_layer_svg` | Export copper layers as SVG + PNG image |
| `export_netlist` | Full component and net connectivity from a schematic |
| `export_bom` | Bill of materials as CSV |
| `run_drc` | Design Rules Check — JSON report + board render |
| `run_erc` | Electrical Rules Check on a schematic |
| `grep_kicad_file` | Substring search in a KiCad file with line context |
| `read_kicad_file` | Read any `.kicad_pcb` or `.kicad_sch` file |
| `read_kicad_section` | Read one named section of a large file |
| `get_component` | One footprint's position, value, and S-expression block |
| `get_board_outline` | Board edge coordinates |
| `get_pad_position` | Absolute pad centre coordinates |
| `get_pin_position` | Schematic pin coordinates |
| `list_footprint_libraries` | All installed `.pretty` libraries |
| `list_footprints_in_library` | Footprints in one library |
| `get_footprint` | Raw S-expression for a footprint |
| `search_footprint` | Search footprints by name across all libraries |

### Edit tools (write to disk — use with care)

Each edit tool that modifies a `.kicad_pcb` file renders the board afterward. Schematic edits render a schematic preview. Use these renders to immediately verify the change before continuing.

| Tool | What it does | Risk |
|---|---|---|
| `add_power_symbol` | Adds a power net symbol — embeds `lib_symbols` definition and places instance atomically | Low — but verify the net name with `list_nets` first |
| `add_trace` | Add a copper segment. Net is a string name, not a number. | **Run `check_trace_clearance` first.** A trace through a pad passes DRC until fill_zones runs |
| `add_wire` | Add a schematic wire | Low — schematic preview shown |
| `add_label` | Add a net label to a schematic | Low |
| `add_component` | Place a footprint in the PCB | Medium — verify position and rotation |
| `add_graphic` | Add a text, line, rect, or circle element | Low |
| `move_component` | Move a footprint to new coordinates | Medium — check for overlaps |
| `move_label` | Move a schematic label | Low |
| `move_symbol` | Move a schematic symbol | Low |
| `replace_footprint` | Swap a footprint in the PCB | Medium — pad count and numbering must match |
| `replace_symbol` | Swap a schematic symbol | Medium |
| `patch_kicad_file` | Exact string replacement in any KiCad file | **High** — operates on raw text; a wrong match corrupts the file |
| `write_kicad_file` | Write an entire file back to disk | **High** — overwrites without diff preview |
| `set_board_outline` | Resize the board boundary | Medium |
| `fill_zones` | Run copper pour fill | Low — safe after routing is verified |
| `cleanup_traces` | Remove redundant segments | Medium — verify with DRC after |
| `cleanup_dangling_wires` | Remove floating wires in schematic | Low |
| `update_pcb_from_schematic` | Sync PCB netlist from schematic | **High** — can reassign or clear pad nets; verify every change in KiCad |
| `autoroute_pcb` | Run FreeRouting autorouter | **High** — treats the whole board; always review result in KiCad |
| `delete_component` | Remove a footprint | High — irreversible without git |
| `delete_graphic` | Remove graphic elements | Medium |
| `create_footprint` | Create a new `.kicad_mod` footprint file | Low |
| `export_fabrication_files` | Generate Gerbers + drill files | Low — production files, verify before ordering |

---

## Recommended workflow

Before any editing session on a board you care about:

```bash
git add -A && git commit -m "checkpoint before AI edits"
```

Then follow this order:

1. **`list_nets`** — discover exact net names before touching anything
2. **`query_pads_in_region`** — inspect the area you intend to route
3. **`check_trace_clearance`** — verify proposed segment is clear
4. **`add_trace`** (or other edit) — make the change
5. **`verify_connectivity`** — confirm the pads are now wired
6. **`run_drc`** — full design rules pass with board render

Open KiCad to visually confirm any change that you can't fully describe from the tool output alone. The MCP tools can tell you coordinates and net names; only KiCad shows you the routing in context.

---

## Known limitations

**`verify_connectivity` false negatives:** connectivity is checked by matching trace endpoints to pad centres using millimetre coordinates rounded to the nearest micron. If a pad position computed from a rotated footprint differs from the trace endpoint by more than 1 µm due to floating-point arithmetic, the BFS will report DISCONNECTED even when the board is correctly routed. Treat DISCONNECTED as "worth checking in KiCad", not as a confirmed fault.

**`check_trace_clearance` layer approximation:** the tool reports all pads near a segment regardless of layer. Through-hole pads affect all layers and are always flagged. SMD pads on the opposite layer are also flagged (conservatively). Use the result as a list of pads to inspect, not as a binary pass/fail.

**`add_power_symbol` requires installed KiCad symbols:** the tool reads the power symbol definition from the system KiCad library (`/usr/share/kicad/symbols/power.kicad_sym`). If KiCad is not installed or the symbol library is in a non-standard location, the tool will fail.

**`update_pcb_from_schematic`** has been observed to report success while assigning zero pads. Always verify pad net assignments with `list_nets` after calling it.
