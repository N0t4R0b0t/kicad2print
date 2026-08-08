---
name: kicad-sexpr-parsing
description: Writing or changing code in kicad2print that scans, parses, or rewrites KiCad S-expression files (src/mcp.rs, src/parser/*). Use when adding a tool that reads .kicad_pcb/.kicad_sch text, fixing a parser, or computing pad/pin coordinates. Covers the trailing-space needle trap, separator checks, Y-down rotation, and the fact that the same computation exists in several places at once.
---

# Parsing KiCad S-expressions in this codebase

Every bug in this area has had the same shape: **wrong output, no error**. Nothing
throws, nothing warns, the result is just quietly incorrect. Assume that failure mode
and design the check accordingly.

## Never write a needle with a trailing space

KiCad 6/7 wrote `(gr_line (start …)` on one line. KiCad 9/10 writes:

```
	(gr_line
		(start 81.28 54.73)
```

So a literal needle like `"(gr_line "` or `"(segment "` matches **nothing** on modern
files — and returns an empty result rather than an error. `get_board_outline` reported
a fabricated 0,0→100,100 board for exactly this reason, for however long it had been
broken.

`for_each_top_level(content, keyword, f)` handles this: it trims a trailing space from
the keyword and requires the next character to be a separator (space, newline, tab,
`(`, `)`, EOF). That also keeps `(gr_text` from matching `(gr_text_box` and `(zone`
from matching `(zone_connect`.

**If you hand-roll a `.find()` instead, apply the separator check yourself.** This is
easy to forget under pressure — a `"(comp "` needle against `(comp\n\t\t\t(ref …)`
slipped into `summarize_netlist` the same day the central fix landed. The test caught
it, printing "Components: none found" while the nets parsed fine.

## The same computation lives in several places

Before fixing anything coordinate-related, **grep for the pattern and fix every hit**.
Patching one implementation and asserting the others are covered is the single most
repeated mistake in this file's history.

Absolute pad position had *three* independent PCB implementations:

| function | reached through |
|---|---|
| `parse_pcb_pads` | `route_net`, `check_trace_clearance`, `verify_connectivity`, `query_pads_in_region` |
| `extract_pad_positions` | `get_pad_position` |
| `collect_pad_positions` | `cleanup_traces` orphan detection |

`grep -n '\* cos_r\|\* sin_r' src/mcp.rs` finds them all.

## Rotation is Y-down — negate the sin terms

```rust
// WRONG — textbook CCW matrix
abs_x = fp_x + dx * cos_r - dy * sin_r;
abs_y = fp_y + dx * sin_r + dy * cos_r;

// CORRECT — KiCad's Y axis points down
abs_x = fp_x + dx * cos_r + dy * sin_r;
abs_y = fp_y - dx * sin_r + dy * cos_r;
```

The wrong sign mirrors every pad to the opposite corner of its footprint: a `-90°`
part reported pads ~21mm from their true location. It is **completely invisible at
rotation 0**, so a test fixture with unrotated footprints proves nothing. Always
include a rotated part.

The schematic pin path (`compute_pin_positions`) is separate code with its own
mirror/Y-flip handling and its own tests. It was already correct — do not "fix" it to
match the PCB side.

## Ground-truth against KiCad, not your own arithmetic

Hand-derived expected values just encode your misunderstanding. `kicad-cli pcb drc
--format json` cites absolute positions for every item it flags — that is an
independent oracle, and it is what exposed the rotation bug. Pin fixtures to the
coordinates KiCad itself reports.

## Verify through the installed binary

`cargo test` passing is necessary, not sufficient. The rotation fix passed 70 unit
tests while two of the three code paths were still wrong, because the test only
exercised one of them. Running the actual installed binary end-to-end is what caught
it:

```bash
cargo install --path . --force
# then drive the real tool over stdio and check the value against KiCad's
```

When you fix something with several call sites, write **one test that asserts on all
of them at once** — the failure mode here is precisely that fixing a single
implementation looks like success.

## Existing helpers worth reaching for

- `for_each_top_level` — top-level block iteration, indent- and layout-agnostic
- `pcb_edit::block_end` — paren-matched end of a block
- `pcb_edit::extract_at` — footprint-level `(at X Y [ROT])`, skipping pad-level ones
- `pcb_edit::find_footprint_blocks` — reference → byte range
- `parse_xy_field`, `parse_quoted_field`, `parse_number_field`

Note `extract_at` takes the first `(at …)` before the first `(pad …)`. That works
because real KiCad writes the footprint's `(at …)` before its properties — a
hand-built fixture with the property first will silently pick up the wrong one.
