---
name: kicad-safe-editing
description: Safety and verification discipline for editing real KiCad boards through the kicad2print MCP tools. Use before and during ANY session that modifies a .kicad_pcb or .kicad_sch the user cares about — routing, footprint swaps, net changes, DRC/ERC cleanup, trace deletion. Covers checkpointing, working on copies, reading DRC deltas rather than absolute counts, not trusting success messages, and knowing when to stop and hand back to KiCad.
---

# Editing a real KiCad board safely

These tools edit KiCad files as structured text. They will happily produce a
syntactically valid board that is electrically wrong, and KiCad will open it
without complaint. The whole job of this skill is to make wrongness visible fast.

## Before touching anything

1. **Checkpoint.** `git add -A && git commit -m "checkpoint before AI edits"` in the
   board's repo. If it isn't a repo, copy the files somewhere first. Several edit
   tools (`delete_trace`, `delete_component`, `patch_kicad_file`, `write_kicad_file`)
   have no undo.
2. **`scan_project`** on the project folder. It is the documented entry point and
   returns the file inventory, BOM and a render in one call.
3. **`list_nets`** before any net-related edit. Never guess or infer a net name from
   the schematic — use the exact string KiCad uses. Auto-generated names like
   `Net-(U1-A1)` and `unconnected-(MOUSE1-Pad2)` are easy to get subtly wrong.

## Experiment on a copy, not the board

When trying something out — a routing strategy, a cleanup pass, anything you might
want to undo — copy the board to a scratch directory first.

**Copy the sibling `.kicad_pro` with it.** DRC reads netclasses, clearances and
per-check severities from the project file. A board copied without it is checked
against stricter defaults: on one real board that was 145 violations with the project
file present versus 157 without — same board, same KiCad, fully deterministic. It
looks exactly like a bug in the report parser if you don't know.

## Read DRC as a delta, never an absolute

Real boards carry pre-existing violations. `run_drc` returning 145 tells you almost
nothing on its own. What matters is what *your* change did:

1. `run_drc` with `raw: true` before the edit, save it
2. make the edit
3. `run_drc` with `raw: true` after
4. diff the violation sets — match on `(type, severity, item descriptions)`

Expect noise even in a clean diff: KiCad sometimes cites a *different witness element*
for the same underlying violation (e.g. "Rectangle of R4" one run, "Segment of R4" the
next). Same net, same track, same defect — not something you caused. Check the nets
involved before believing a violation is yours.

## Do not trust a success message

Verify the effect, not the report:

- `update_pcb_from_schematic` has been observed reporting success while assigning
  **zero** pads. Always follow with `list_nets`.
- After any edit that should connect things, `verify_connectivity` or a DRC
  `unconnected_items` count is the real evidence.
- After `replace_footprint`, re-check `list_nets` — net carryover matches by pad
  number and silently assumes the old and new numbering share intent.

## Delegate the mechanical calls

Hand tool-heavy, already-decided work to the **`kicad-worker`** subagent: DRC/ERC
runs, reads and greps, position and net lookups, renders, specific edits. It reports
a condensed digest instead of dumping payloads into the main conversation.

Give it decided work, not decisions. Choosing between footprint candidates,
diagnosing why a route failed, or weighing a trade-off stays in the main thread —
hand the agent the resulting concrete action.

## Know when to stop

If the board is getting worse with each call, stop. Open KiCad, undo manually, and
reconsider. That is not a failure state; it is the intended escape hatch. These tools
cannot see mechanical constraints, thermal requirements, or why a trace was
deliberately routed the long way round.

Two specific things they cannot judge, where you should defer to the user:

- Whether a violation is *intentional* — plenty of working boards ship with
  clearance warnings the designer accepted knowingly.
- Whether copper that overlaps a pad is a bug or a deliberate stitch. On one real
  board, foreign-net copper sat exactly on a signal pad; the right move was to report
  it and stop, not to route around it or "fix" it.
