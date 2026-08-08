---
name: kicad-pcb-routing
description: Placing and routing a KiCad PCB with the kicad2print MCP tools — route_net, layer conventions, net ordering, via strategy, board outline, zone fills, and reading route failures. Use when routing traces, connecting pads, fixing unconnected nets, placing or moving footprints, or preparing a board for fabrication.
---

# Routing and laying out a PCB

Read `kicad-safe-editing` first if you are touching a board that matters.

## Route with `route_net`, not hand-built segments

`route_net(path, from_ref, from_pad, to_ref, to_pad)` runs an octilinear (45°) A*
server-side: it avoids existing pads and traces at full clearance, drops vias to
change layers when it needs to, and writes every segment itself. One call replaces a
whole polyline of `add_trace` calls with hand-computed coordinates.

Useful options: `layer` (omit to let it use both sides), `width`, `clearance`,
`grid` (default 0.635mm — a quarter of the 2.54mm through-hole pitch),
`via_cost` (default 2.5mm-equivalent), `dry_run`, `max_length_ratio` (default 3.0).

Always `dry_run: true` first on anything non-trivial. It reports segment count, via
count, length, and the ratio to straight-line distance — enough to judge the route
before committing it.

`add_trace` is still right for a *specific* segment you want shaped a particular way.
Pass `check: true` and the `net` so same-net T-junctions aren't false-flagged.

## Net order is the thing that actually decides success

`route_net` handles one connection at a time and knows nothing about the ones you
haven't asked for yet. Routing a board therefore depends on the order you choose — an
early net can wall off a corridor a later net needs, and you only find out when the
later route fails or returns a wild detour.

- Power and wide traces first. They need the room and tolerate detours worst.
- Decide a layer convention up front and pass `layer` explicitly to enforce it. The
  classic two-layer habit is one side for horizontal runs, the other for vertical.
- Route a chain pad-to-pad for multi-pad nets (A→B, then B→C). Same-net copper is not
  an obstacle, so the second leg can tee into the first.
- Leave the congested middle of the board until you understand what has to cross it.

## Vias, and why through-hole boards often need none

A through-hole pad is plated through every copper layer, so it is a **free layer
change**: the router seeds its search on every layer a THT pad touches. On a
through-hole board many nets route with zero vias for that reason.

Where a real via is needed, it is costed at `via_cost` and its site is checked for
clearance on *all* layers. Do not force single-layer routing to avoid vias — on a
congested board that turned a 15.9mm hop into a 99.2mm detour around the perimeter.
Lower `via_cost` if you want shorter routes and accept more vias; raise it to make
the router work harder to stay on one side.

## Reading a failure

The error text is diagnostic — read it rather than immediately retrying.

- **`explored 2 nodes`** — the *start pad is boxed in*. The router could not take a
  single step off it. Something is sitting on or immediately around that pad; go look
  at what, with `query_traces_in_region` / `query_pads_in_region`. Retrying with a
  finer grid will not help.
- **`closest approach 0.60mm`** — it got essentially to the destination and the final
  approach is blocked. Usually worth a finer `grid` (e.g. `0.3175`), or freeing the
  pad's escape.
- **`explored 249134 nodes`** with a large closest approach — genuinely no corridor.
  Rip something up or move a part.
- **Rejected for length ratio** — the direct corridor is blocked and the only route
  goes around the board. Treat that as a signal to rip up the blocker, *not* as a
  reason to raise `max_length_ratio`. Raise it only when you have looked at the path
  and decided the detour is acceptable.

## Placement, outline, fills

- `add_component(library, footprint, reference, value, x, y, rotation)` places a
  footprint; `move_component` takes either absolute `x`/`y` or relative `dx`/`dy`.
- `set_board_outline(x_min, y_min, x_max, y_max)` replaces the Edge.Cuts rectangle;
  `update_zones` keeps the copper pour matching. `get_board_outline` first — if it
  reports a suspiciously round number like 0,0→100,100, the outline may not have
  parsed rather than actually being that size.
- `fill_zones` after routing is verified, not before — a fill over an unfinished
  route hides what you are looking at.

## Finishing

1. `verify_connectivity` on the pads you routed, or check DRC `unconnected_items`
2. `run_drc` and compare against the pre-edit report as a delta
3. `fill_zones`
4. `export_fabrication_files(output_dir, layers, zip)` — production output; verify
   in KiCad before ordering
