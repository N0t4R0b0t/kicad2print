---
name: kicad-project-creation
description: Starting a brand-new KiCad project and taking it from empty to fabrication with the kicad2print MCP tools. Use when creating a board from scratch, scaffolding a new .kicad_sch or .kicad_pcb, or when asked what order to build a design in. Explains what the tools genuinely cannot scaffold and where KiCad itself is still required.
---

# Starting a new KiCad project

## First, the honest limitation

**There is no project-creation tool.** Every edit tool reads an existing file and
errors if it is missing. `create_footprint` makes a `.kicad_mod`, nothing more.

`write_kicad_file` *can* create a file from scratch (it creates parent directories
too), so a minimal skeleton is possible:

```
(kicad_pcb (version 20240108) (generator "kicad2print"))
```

But a file written this way has no layer stack, no netclasses, no design rules, and
no `.kicad_pro` — which means DRC will run against defaults and report differently
from a real project, and KiCad may need to repair it on open.

**The reliable path is to create the project in KiCad** (File → New Project), which
gives you a correct `.kicad_pro`, layer table and design rules, then drive the empty
schematic and board with these tools. Recommend that unless the user explicitly wants
a hand-scaffolded file and understands the trade.

If the user does want everything from scratch without opening KiCad, say plainly that
the result will need a pass in KiCad before it is trustworthy, then proceed.

## Build order

Schematic first. The PCB inherits nets from it, and fixing net structure after
routing is far more work than before.

**1. Schematic** — see `kicad-schematic-editing`
   - `add_symbol` each part with its `lib_id`, reference, value, and ideally its
     `footprint` at placement time
   - `add_wire` between real `get_pin_position` coordinates
   - `add_label` for signal nets, `add_power_symbol` for rails
   - `run_erc` until clean, `render_schematic` to read it

**2. Footprint assignment**
   - Set `footprint` on `add_symbol`, or fill it in afterwards
   - `search_footprint` / `list_footprints_in_library` to find the right one
   - `get_footprint` to check pad numbering matches the symbol's pins — this is where
     a mismatch becomes a wrong net later

**3. PCB scaffold**
   - `update_pcb_from_schematic` syncs footprint names and values and reports what is
     missing from the board. It does **not** place footprints — you still
     `add_component` each one. Follow with `list_nets` and confirm pads carry nets;
     it has been seen reporting success having assigned zero.
   - `set_board_outline` to the real mechanical envelope

**4. Placement**
   - `move_component` to arrange. Placement decides routability more than routing
     technique does — connectors at the edges they physically reach, decoupling caps
     next to their pins, related parts adjacent.
   - `run_drc` early to catch `courtyards_overlap` before you route around a bad
     placement

**5. Routing** — see `kicad-pcb-routing`
   - Power and wide nets first, then signals
   - `route_net` with a deliberate layer convention

**6. Finish**
   - `fill_zones`
   - `run_drc` and read the whole list — on a new board every violation is yours
   - `export_fabrication_files`, then verify in KiCad before ordering

## What to check that a new board makes easy to miss

- **Board outline actually parsed.** `get_board_outline` returning a round
  0,0→100,100 usually means no outline was found, not that the board is 100mm square.
- **Every net is intentional.** On a new board, `list_nets` should contain no
  surprises. `unconnected-(REF-PadN)` entries are pins you never wired.
- **DRC from zero.** A mature board carries accepted violations; a new one should not.
  Treat the first clean-ish DRC as the baseline you defend from then on.
