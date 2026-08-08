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

## Setup

### As a Claude Code plugin (recommended)

This repo self-hosts a plugin marketplace, which is the only setup that bundles the subagent alongside the MCP tools:

```
/plugin marketplace add N0t4R0b0t/kicad2print
/plugin install kicad2print@kicad2print
```

The plugin points at the `kicad2print` binary **by name**, so it must already be on your `PATH` — the plugin does not bundle it. `.claude-plugin/plugin.json` references `./.mcp.json` for the server definition.

Under a plugin install the tools are namespaced `mcp__plugin_kicad2print_kicad2print__*`, and the agent appears as `kicad2print:kicad-worker`.

### Without the plugin

Register the server directly — in `~/.claude.json` for Claude Code, or `~/.config/Claude/claude_desktop_config.json` for Claude Desktop:

```json
{
  "mcpServers": {
    "kicad2print": {
      "command": "/absolute/path/to/kicad2print",
      "args": ["--mcp"]
    }
  }
}
```

Here the tools are namespaced `mcp__kicad2print__*`. Claude Desktop has no concept of agents, so this route gives you the tools only.

The server does not hot-reload. After rebuilding the binary, reconnect the server (`/mcp reconnect`) or restart the session, or you will keep talking to the old build.

### Bundled skills

The plugin ships six skills under `skills/`, auto-discovered on install. They load
when the task matches their description, so you get the relevant one without asking:

| Skill | Loads when |
|---|---|
| `kicad-safe-editing` | any edit to a board that matters — checkpointing, working on copies, reading DRC as a delta, when to stop |
| `kicad-pcb-routing` | routing, placement, board outline, zone fills, and reading `route_net` failures |
| `kicad-schematic-editing` | symbols, wires, labels, power symbols, ERC, staying on the connection grid |
| `kicad-project-creation` | starting a board from scratch, and what the tools genuinely cannot scaffold |
| `kicad-sexpr-parsing` | *contributors* — changing kicad2print's own KiCad file parsing |
| `mcp-tool-output` | *contributors* — designing tool responses that don't burn the caller's context |

The last two are scoped to development of this repo and stay quiet during ordinary
board work.

### The `kicad-worker` subagent

`agents/kicad-worker.md` is a Haiku subagent scoped to the kicad2print MCP tools. It executes mechanical, already-decided work — DRC/ERC runs, file reads and greps, position and net lookups, renders, specific edits — and reports back a condensed digest instead of raw tool output.

**Why it exists:** these tools return large payloads. A DRC report on a modest board is thousands of tokens; a netlist or layer SVG is tens of thousands. Routing that through a cheap subagent keeps it out of the main conversation, where it would otherwise crowd out the actual design reasoning.

Its `tools:` list uses wildcards covering **both** namespaces, so the same file works whether the server is installed as a plugin or registered directly:

```yaml
tools: ["Read", "Grep", "Glob", "Bash", "mcp__kicad2print__*", "mcp__plugin_kicad2print_kicad2print__*"]
```

Prefer wildcards over an explicit tool list — an enumerated allowlist silently hides any newly added tool from the agent, which then reports it as unavailable rather than failing loudly.

If you are not using the plugin, copy the file to `~/.claude/agents/` to make the agent available. Agent definitions are read at session start, so reload after adding or editing one.

**Give it decided work, not decisions.** It is deliberately not the right tool for choosing between footprint candidates, diagnosing why a route failed, or weighing trade-offs. Resolve those in the main conversation and hand the agent the resulting concrete action.

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
| `query_pads_in_region` | All pads whose centre falls inside a bounding box — use before routing. Optional `layer` filter is real (matches per-pad copper layers, THT pads match any layer). |
| `query_traces_in_region` | All copper trace segments whose bounding box overlaps a region — pairs with `query_pads_in_region` for pre-routing reconnaissance. Uses bounding-box overlap (conservative: a shallow diagonal segment can rarely be reported when only its bbox, not the segment itself, clips the region). |
| `check_trace_clearance` | Collision and clearance check for a proposed segment — call before `add_trace`. Checks the segment against **both** existing pads and existing traces (different-layer and same-net traces are exempted; pass `net` so same-net T-junctions aren't false-flagged). |
| `verify_connectivity` | BFS through traces and vias to confirm two pads are physically wired |
| `export_layer_svg` | Export copper layers as a PNG preview image plus the path to the saved SVG. The SVG markup is **not** inlined by default (it runs to tens of thousands of tokens) — grep the saved file for exact coordinates, or pass `include_svg_source: true`. |
| `export_netlist` | Condensed component list (ref, value, footprint) and every net with its pads. Pass `raw: true` for the full kicad-cli S-expression, which is ~10x larger and mostly library boilerplate. |
| `export_bom` | Bill of materials as CSV |
| `run_drc` | Design Rules Check — violations grouped by type with counts, capped by `max_details` (default 50). Pass `raw: true` for the full JSON, `include_render: true` for a board image (off by default). Reads design rules from the sibling `.kicad_pro`; a board copied without it is checked against stricter defaults and reports different counts. |
| `run_erc` | Electrical Rules Check on a schematic — same condensed grouping as `run_drc`, with `raw` and `max_details`. |
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
| `route_net` | **Route a whole net between two pads with one call.** Server-side octilinear (45°) A* that avoids existing pads and traces at proper clearance, switches layers through vias when needed, and writes every segment itself. Omit `layer` to let it use both sides; `dry_run: true` previews the path. Rejects a route longer than `max_length_ratio` (default 3.0) × the direct distance rather than committing a board-spanning detour. | Medium — prefer this over emitting many `add_trace` calls; still verify with `run_drc` |
| `add_trace` | Add a copper segment. Net is a string name, not a number. Optional `check: true` runs the same collision check as `check_trace_clearance` before writing and refuses on any COLLISION (add `force: true` to write anyway); default is `check: false`, matching prior always-write behavior. | **Pass `check: true`** for routing sessions. A trace through a pad passes DRC until fill_zones runs |
| `add_wire` | Add a schematic wire | Low — schematic preview shown |
| `add_label` | Add a net label to a schematic | Low |
| `add_component` | Place a footprint in the PCB | Medium — verify position and rotation |
| `add_graphic` | Add a text, line, rect, or circle element | Low |
| `move_component` | Move a footprint to new coordinates | Medium — check for overlaps |
| `move_label` | Move a schematic label | Low |
| `move_symbol` | Move a schematic symbol | Low |
| `replace_footprint` | Swap a footprint in the PCB. Nets are carried over automatically by matching pad number between old and new footprints; the response reports the carryover count and flags any pad number with no old-side match | Medium — carryover assumes old/new pad numbering share intent, same ambiguity as KiCad's own "Change footprint"; verify with `list_nets` after a swap between dissimilar footprints |
| `replace_symbol` | Swap a schematic symbol | Medium |
| `delete_trace` | Remove trace segments by `net`, `layer`, `uuid`, or a region (all combine as AND). Supports `dry_run: true` to preview what would be removed without writing. Refuses to run with no filter given. | **High** — irreversible without git, same tier as `delete_component` |
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
2. **`query_pads_in_region`** and **`query_traces_in_region`** — inspect the area you intend to route, both pads and existing copper
3. **`route_net`** — for an ordinary pad-to-pad connection, let the router compute the geometry. Use `dry_run: true` first to see the path and its length ratio before committing.
4. **`add_trace`** (with `check: true`, passing the trace's `net`) — only when you need a specific segment the router wouldn't choose, e.g. a deliberate shape or a partial run. `check_trace_clearance` does the same collision test standalone.
5. **`verify_connectivity`** — confirm the pads are now wired
6. **`run_drc`** — full design rules pass

**On net order:** `route_net` handles one connection at a time and knows nothing about the ones you haven't asked for yet. Routing a board from scratch therefore depends on the order you pick — an early net can wall off a corridor a later net needs, and you find out only when the later route fails or returns a wild detour. Route power and wide traces first, decide a layer convention up front (e.g. pass `layer` explicitly to keep one side for horizontal runs), and treat a rejected over-long route as a signal to rip something up rather than a reason to raise `max_length_ratio`.

If routing goes wrong, use **`delete_trace`** (filtered by `net`, `layer`, `uuid`, or region, with `dry_run: true` to preview first) to remove the bad segments — don't fall back to an ad-hoc script over the raw file.

Open KiCad to visually confirm any change that you can't fully describe from the tool output alone. The MCP tools can tell you coordinates and net names; only KiCad shows you the routing in context.

---

## Known limitations

**Pad positions on rotated footprints were wrong before 2026-07-30.** The rotation used a plain counter-clockwise matrix, but the PCB Y axis points down, so the sin terms must be negated. Every rotated footprint had its pads mirrored to the opposite corner — a `-90°` part reported pads ~21 mm from their true location — while unrotated parts were unaffected, which is why it went unnoticed.

The PCB side had **three independent implementations** of "absolute pad position" and all three carried the bug: `parse_pcb_pads` (behind `route_net`, `check_trace_clearance`, `verify_connectivity`, `query_pads_in_region`), `extract_pad_positions` (behind `get_pad_position`), and `collect_pad_positions` (behind `cleanup_traces`' orphan detection — so it could delete a segment that really did land on a rotated footprint's pad). All are fixed and pinned by a single test that checks every path against KiCad's own reported coordinates. The schematic pin path is separate code with its own Y-flip handling and was already correct.

If you acted on pad coordinates from an older build, re-check them.

**`verify_connectivity` false negatives:** connectivity is checked by matching trace endpoints to pad centres using millimetre coordinates bucketed to a 5 micron grid. If a pad position computed from a rotated footprint differs from the trace endpoint by more than 5 µm — well beyond ordinary floating-point drift, but possible in unusual cases — the BFS will report DISCONNECTED even when the board is correctly routed. Treat DISCONNECTED as "worth checking in KiCad", not as a confirmed fault.

**`run_drc` shorting_items / solder_mask_bridge positions can be misleading after a footprint pad edit:** `run_drc` shells out to `kicad-cli pcb drc` fresh on every call with no caching — the position mismatch is an upstream `kicad-cli`/pcbnew JSON-reporting characteristic, not a kicad2print bug. After repositioning or swapping pads (e.g. via `patch_kicad_file` or `replace_footprint`), a reported violation can name a real pad at a position that pad's current geometry could never produce. If a DRC-reported position looks physically impossible, cross-check with `verify_connectivity` — it inspects the file directly and is the more reliable source in that situation.

**`check_trace_clearance` layer approximation:** the tool reports all pads near a segment regardless of layer. Through-hole pads affect all layers and are always flagged. SMD pads on the opposite layer are also flagged (conservatively). Use the result as a list of pads to inspect, not as a binary pass/fail.

**`add_power_symbol` requires installed KiCad symbols:** the tool reads the power symbol definition from the system KiCad library (`/usr/share/kicad/symbols/power.kicad_sym`). If KiCad is not installed or the symbol library is in a non-standard location, the tool will fail.

**`update_pcb_from_schematic`** has been observed to report success while assigning zero pads. Always verify pad net assignments with `list_nets` after calling it.
