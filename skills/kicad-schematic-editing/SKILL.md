---
name: kicad-schematic-editing
description: Editing and tidying KiCad schematics with the kicad2print MCP tools — placing symbols, wiring pins with real orthogonal wires rather than label islands, power symbols, swapping symbols, running ERC, and keeping the sheet on-grid and readable. Use when adding or moving components on a .kicad_sch, connecting pins, naming nets, fixing ERC violations, or cleaning up a schematic layout.
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

## Wire it — do not build label islands

**The most common way these tools wreck a schematic is by placing a part in isolation
and connecting it with labels instead of wires.** It is electrically valid and it
passes ERC, and it destroys the thing a schematic exists for: showing the circuit at a
glance. A reader now has to text-search the sheet to discover what a resistor is even
attached to.

The failure signature is a symbol whose pins *all* terminate in labels:

```
        island (bad)                    wired (good)

     ┌──────┐                         ┌──────┐        ┌──────┐
 CLK─┤ R1   ├─Net-(U1-A1)          ───┤ R1   ├────────┤ U1   │
     └──────┘                         └──────┘        └──────┘
```

It happens because labelling is *one call* and wiring takes a coordinate calculation
plus two. That is not a good enough reason. Wire it.

### Default: draw the wire

For any two pins on the same sheet with a reasonable corridor between them, draw an
orthogonal wire. Get both endpoints from `get_pin_position`, then emit an L (two
segments) or a Z/dogleg (three):

```
L:  add_wire(x1, y1, x1, y2)      # leave the pin, then turn
    add_wire(x1, y2, x2, y2)      # run in to the target pin

Z:  add_wire(x1, y1, xm, y1)      # xm = a clear column between the two
    add_wire(xm, y1, xm, y2)
    add_wire(xm, y2, x2, y2)
```

Choose the variant whose corner lands in empty space rather than on top of a symbol.
**The first segment must continue the direction the pin points** — a wire that
immediately doubles back across its own symbol body reads as a mistake even when it
is connected correctly.

### Placement is what makes wiring possible

If a wire is awkward, the placement is usually wrong — fix that first rather than
reaching for a label. Place a part next to what it connects to, rotated so its pins
face the target. A series resistor belongs *between* the two things it sits between,
not parked in a corner with `CLK` written on both ends.

Do this before wiring, with `move_symbol`, and the wire becomes trivial.

### When a label genuinely is the right call

Labels are correct — not a fallback — in these cases:

- **Power and ground.** Use `add_power_symbol`, never a wire dragged across the
  sheet. This is the universal convention and carries meaning at a glance.
- **A run that would cross a thicket.** On a busy sheet, a wire that has to cross
  many unrelated wires to get there is *less* readable than a named label at each
  end. This is the "very busy design" exception and it is real — but it is a
  judgement about the specific corridor being congested, not a general licence.
- **Buses and repeated signals** fanning out to many destinations — `D0..D7` to
  three chips is unreadable as wires.
- **Off-sheet connections**, where a wire is not an option.
- **Long-haul signals** genuinely spanning opposite ends of a large sheet.

A useful check: if a label's two endpoints are close enough that you could draw the
wire without crossing anything, the label is laziness. If drawing it would mean
threading past a dozen other nets, the label is doing its job.

### Sanity-check yourself

After a batch of schematic edits, `render_schematic` and *look at it*. Then ask:

- Is any symbol connected **only** by labels? That is an island — go wire it.
- Roughly what fraction of connections on this sheet are label-only? Beyond about a
  quarter, on a sheet that isn't dense, you have built a net-list-as-diagram.
- Would someone reading this see the circuit's shape, or have to reconstruct it by
  matching strings?

## Naming nets

Labels have a second, entirely legitimate job that has nothing to do with the above:
**naming**. A wired net still benefits from a meaningful name.

- **`add_label`** — gives a net a real name (`CLK`, `DATA`) instead of letting KiCad
  auto-generate `Net-(U1-A1)`. Attaching a label to an already-wired net is good
  practice; using one *instead of* the wire is the anti-pattern above. The label must
  sit on the wire — floating nearby is a `label_dangling` ERC error.
- **`add_power_symbol`** — for rails (`GND`, `VCC`, `+5V`).

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

- **Signal flow left to right, power top to bottom.** Inputs on the left, outputs on
  the right. This is the strongest single readability lever.
- Wires orthogonal; avoid diagonal runs except where they genuinely aid clarity.
- Power symbols point up, ground symbols point down — the convention carries meaning
  at a glance.
- Labels sit on the wire, reading left-to-right.
- Group by function, not by package. The sheet should show the circuit's structure —
  a decoupling cap belongs beside the pin it decouples, not in a row with the other
  capacitors.
- Leave room. A sheet packed to the edges forces exactly the label-island compromise
  described above; spreading parts out costs nothing and keeps wires drawable.

## Pushing to the PCB

`update_pcb_from_schematic` syncs **footprint names and values only**. It does not
place new footprints, reposition anything, or reroute. Components present in the
schematic but missing from the PCB are *reported*, not created — you still place them
with `add_component`.

It has also been observed reporting success while assigning zero pads. Always follow
it with `list_nets` on the PCB and confirm the pads actually carry the nets you
expect.
