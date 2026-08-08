---
name: kicad-schematic-editing
description: Editing and tidying KiCad schematics with the kicad2print MCP tools — placing symbols, wiring pins, labels and power symbols, swapping symbols, running ERC, and keeping the sheet on-grid and readable. Use when adding or moving components on a .kicad_sch, connecting pins, naming nets, fixing ERC violations, or cleaning up a schematic.
---

# Editing a KiCad schematic

Read `kicad-safe-editing` first if the schematic matters. Schematic edits are the
upstream of everything: a wrong net here becomes a wrong net on the PCB.

## The tools and their real signatures

```
add_symbol(path, lib_id, reference, value, x, y, [footprint], [rotation])
add_wire(path, x1, y1, x2, y2)
add_label(path, text, x, y, [label_type], [global_shape], [rotation])
add_power_symbol(path, net_name, x, y, [rotation])
move_symbol / move_label / delete_symbol / delete_wire / delete_label
replace_symbol, cleanup_dangling_wires
get_pin_position, get_schematic_net, run_erc, render_schematic
```

`lib_id` is the full library-qualified id — `"Device:R"`, `"Connector:USB_B_Mini"` —
not a bare symbol name. `add_symbol` embeds the `lib_symbols` definition and places
the instance together, so it does not leave a half-placed symbol behind.

`add_power_symbol` reads from the system KiCad symbol library
(`/usr/share/kicad/symbols/power.kicad_sym`). If KiCad isn't installed or lives
somewhere non-standard, it fails — that's an environment problem, not a bad call.

## Work from real pin coordinates

Never estimate where a pin is. `get_pin_position` gives the absolute coordinates,
already accounting for the symbol's rotation and mirroring, and those are what
`add_wire` needs. The schematic pin path handles a Y-flip and `mirror x`/`mirror y`
that are easy to get wrong by hand.

Wire endpoints must land exactly on pin coordinates. "Close enough" produces a
schematic that looks connected and isn't — and ERC will tell you only if the endpoint
also happens to be off-grid.

## Stay on the connection grid

KiCad's default schematic connection grid is 1.27mm (50 mil). Wire ends and pins that
fall off it produce `endpoint_off_grid` ERC violations — "Symbol pin or wire end off
connection grid" — and, worse, silently fail to connect. Sub-micron drift is enough:
real boards show these on wires 0.0039mm long, which are leftovers from dragging.

Place symbols and route wires on multiples of 1.27mm unless you have a reason not to,
and treat any `endpoint_off_grid` result as a genuine connectivity risk rather than a
cosmetic warning.

## Naming nets

Two ways, and they mean different things:

- **`add_label`** — a local or global net label. Use it to give a net a meaningful
  name (`CLK`, `DATA`) instead of letting KiCad auto-generate `Net-(U1-A1)`. Attach it
  to the wire; a label floating near a wire is a `label_dangling` ERC error.
- **`add_power_symbol`** — for rails (`GND`, `VCC`, `+5V`). These are the conventional
  way to express power connectivity without drawing wires across the whole sheet.

Check the result with `get_schematic_net(path, reference, pin)`, which reports which
net a pin actually landed on.

## After structural edits

- `replace_symbol` can leave wires attached to pins that no longer exist. Run
  **`cleanup_dangling_wires`** afterwards.
- `run_erc` — condensed and grouped by type by default; `raw: true` for the full
  JSON, `max_details` to widen the itemised list.
- `render_schematic` to actually look at it. A schematic that reads badly to a human
  is a schematic that will be maintained badly, regardless of what ERC says.

## Keeping it readable

- Wires orthogonal; avoid diagonal runs except where they genuinely aid clarity.
- Power symbols point up, ground symbols point down — the convention carries meaning
  at a glance.
- Labels sit on the wire, reading left-to-right.
- Group by function, not by package. The sheet should show the circuit's structure.

## Pushing to the PCB

`update_pcb_from_schematic` syncs **footprint names and values only**. It does not
place new footprints, reposition anything, or reroute. Components present in the
schematic but missing from the PCB are *reported*, not created — you still place them
with `add_component`.

It has also been observed reporting success while assigning zero pads. Always follow
it with `list_nets` on the PCB and confirm the pads actually carry the nets you
expect.
