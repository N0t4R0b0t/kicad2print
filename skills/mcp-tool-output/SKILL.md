---
name: mcp-tool-output
description: Designing or reviewing MCP tool output so it doesn't burn the caller's context. Use when writing a tool that returns a generated file, report, image, or query result, or when an existing tool's responses feel expensive. Covers condensed-by-default with a raw escape hatch, returning paths instead of contents, opt-in images, and measuring the actual token cost.
---

# MCP tool output that doesn't burn context

An MCP tool's return value lands directly in the caller's context window. A tool that
returns a whole generated file is a token bug waiting to happen — and unlike a slow
tool, nothing surfaces the cost. You find out when the conversation runs out of room.

## The rule

**Condensed by default, `raw: true` to opt out.** Never the reverse. The common case
should be cheap; the caller who genuinely needs the full payload can ask.

Measured on one real project:

| tool | before | after |
|---|---|---|
| netlist export | ~9,979 tokens | ~889 tokens |
| DRC report | full JSON every call | grouped by type, capped detail |
| layer SVG | 110 KB inlined, truncated | image + path, ~26 tokens of text |

## Condense by dropping structure, not by truncating

Truncation loses the end of the data and tells the caller nothing about what's
missing. Condensing means understanding the payload and keeping the part that answers
the question.

A KiCad netlist is ~40 KB, of which roughly two-thirds is per-component metadata and
a `libparts` section restating every library symbol. None of it answers "what
connects to what". One line per component and one line per net keeps everything that
matters:

```
Components (15):
  R1   10K    Resistor_THT:R_Axial_DIN0207_L6.3mm_P7.62mm
Nets (45):
  CLK   [3] MOUSE1.5, R1.2, U1.5
  GND  [17] C2.2, C3.2, D2.2, JP1.2, ...
```

That is smaller *and* more readable than the raw form. If your condensed output is
harder to read than the original, you've compressed rather than condensed.

## Cap detail, never cap counts

When a report has many items, cap the itemised list — but always report complete
totals and per-category counts, and say what was withheld:

```
DRC: 145 violation(s) (108 error / 37 warning)
By type:
  clearance: 43
  solder_mask_bridge: 27
  ...
... 95 more violation(s) not shown (raise max_details or set raw=true)
```

The caller can now decide whether they need more. A bare truncation can't be reasoned
about.

## Return a path, not the contents

For anything file-shaped — SVG, gerbers, exports — write the file, keep it, and return
the path plus a size. The caller can grep it for exactly the coordinates they need
instead of paying for the whole thing.

**Do not delete the file after inlining it.** One tool inlined 110 KB of SVG (which
then truncated) and *then* removed the file, leaving no way to recover the data it
had just failed to deliver.

## Images are opt-in

A base64 image is thousands of tokens and cannot be skimmed. Attaching one to every
call of an edit or report tool is pure cost when the caller only wanted a number.
Make it `include_render: true`, defaulting off, and mention in the tool description
that it exists.

## Strip identifiers nothing will act on

UUIDs, `$schema`, timestamps, generator versions, internal handles — if the caller
cannot do anything with a field, it is noise. Keep what is load-bearing: for a
violation that's the type, severity, human-readable message, and a location or
component reference.

## Measure it

Don't estimate. Call the tool, capture the response, and count:

```bash
printf 'chars: %d  (~%d tokens)\n' $(wc -c < out.txt) $(( $(wc -c < out.txt) / 4 ))
```

Do it before and after. A change that "feels lighter" and a 91% reduction are
different claims, and only one of them is worth putting in a commit message.

## Say so in the description

The tool description is where the caller learns the escape hatch exists. State the
default, name the flag, and say why:

> …returns a condensed summary of violations grouped by type (pass `raw=true` for the
> full JSON report, `include_render=true` for a board image)
