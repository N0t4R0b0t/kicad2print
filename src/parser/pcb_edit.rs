// Copyright (c) 2024 Ricardo Salvador
// Licensed under the GNU Affero General Public License v3.0

// Public editing API — functions are part of the MCP tool surface, not all called internally.
#![allow(dead_code)]

//! Text-based PCB S-expression editing utilities.
//!
//! Operates directly on raw file content using byte-offset ranges, so that
//! unchanged portions of the file are preserved verbatim (whitespace, comments, etc.).

use std::collections::HashMap;
use std::ops::Range;

// ---------------------------------------------------------------------------
// Block discovery
// ---------------------------------------------------------------------------

/// Returns byte ranges of every footprint block in a PCB file, keyed by reference.
///
/// Each range spans from the opening `(footprint ` to the matching `)`, inclusive.
pub fn find_footprint_blocks(content: &str) -> HashMap<String, Range<usize>> {
    let mut result = HashMap::new();
    // Top-level footprint blocks are at the first indent level inside kicad_pcb.
    // KiCad 6/7 uses two spaces; KiCad 9/10 uses one tab. Search for both.
    // We must NOT match deeper (footprint ...) nodes (e.g. inside lib_symbols),
    // so we require the line prefix to be exactly one level of indent.
    let mut pos = 0;
    while let Some(rel) = content[pos..].find("(footprint ") {
        let fp_start = pos + rel;

        // Walk back to the start of this line and check the prefix is a single
        // indent unit (two spaces OR one tab) — no more, no less.
        let line_start = content[..fp_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = &content[line_start..fp_start];
        let is_top_level = prefix == "  " || prefix == "\t";

        if is_top_level {
            let end = block_end(content, fp_start);
            let block = &content[fp_start..end];
            if let Some(reference) = fp_text_value(block, "reference") {
                result.insert(reference, fp_start..end);
            }
            pos = end;
        } else {
            pos = fp_start + 1;
        }
    }
    result
}

/// Returns the byte index just past the closing `)` of the parenthesized block at `start`.
pub fn block_end(content: &str, start: usize) -> usize {
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut depth = 0i32;
    let mut i = start;

    while i < len {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'(' => { depth += 1; i += 1; }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 { return i; }
            }
            _ => { i += 1; }
        }
    }
    i
}

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

/// Extract the string from `(fp_text KIND "VALUE" ...)` in a footprint block.
pub fn fp_text_value(block: &str, kind: &str) -> Option<String> {
    // KiCad 6: (fp_text reference "VALUE" ...)
    let marker6 = format!("fp_text {} \"", kind);
    if let Some(pos) = block.find(&marker6) {
        let after = &block[pos + marker6.len()..];
        if let Some(end) = after.find('"') {
            return Some(after[..end].to_string());
        }
    }
    // KiCad 7+: (property "Reference" "VALUE" ...) or (property "Value" "VALUE" ...)
    let prop_key = match kind {
        "reference" => "Reference",
        "value"     => "Value",
        other       => other,
    };
    let marker7 = format!("(property \"{}\" \"", prop_key);
    if let Some(pos) = block.find(&marker7) {
        let after = &block[pos + marker7.len()..];
        if let Some(end) = after.find('"') {
            return Some(after[..end].to_string());
        }
    }
    None
}

/// Extract the `lib:name` string from `(footprint "lib:name" ...`.
pub fn extract_fp_name(block: &str) -> Option<String> {
    let prefix = "(footprint \"";
    let pos = block.find(prefix)?;
    let after = &block[pos + prefix.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Extract `(x, y, rotation)` from the footprint-level `(at X Y [ROT])`.
///
/// Searches only in the region before the first `(pad ` to avoid matching
/// pad-level `(at ...)` nodes.
pub fn extract_at(block: &str) -> Option<(f64, f64, f64)> {
    let search_end = block.find("(pad ").unwrap_or(block.len());
    let region = &block[..search_end];
    let pos = region.find("(at ")?;
    let after = &region[pos + 4..];
    let close = after.find(')')?;
    let args: Vec<f64> = after[..close]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match args.as_slice() {
        [x, y]       => Some((*x, *y, 0.0)),
        [x, y, rot]  => Some((*x, *y, *rot)),
        _            => None,
    }
}

/// Extract the tstamp UUID from a footprint block's `(tstamp UUID)` node.
pub fn extract_tstamp(block: &str) -> Option<String> {
    let pos = block.find("(tstamp ")?;
    let after = &block[pos + 8..];
    let end = after.find(')')?;
    Some(after[..end].trim().to_string())
}

// ---------------------------------------------------------------------------
// Replacement helpers
// ---------------------------------------------------------------------------

/// Replace the footprint-level `(at X Y [ROT])` with new coordinates.
pub fn replace_at(block: &str, x: f64, y: f64, rot: f64) -> String {
    let pad_start = block.find("(pad ").unwrap_or(block.len());
    let (prefix, suffix) = block.split_at(pad_start);

    let new_at = fmt_at(x, y, rot);
    if let Some(at_pos) = prefix.find("(at ") {
        let rel_close = prefix[at_pos + 4..].find(')').unwrap_or(0);
        let at_end = at_pos + 4 + rel_close + 1;
        return format!("{}{}{}{}", &prefix[..at_pos], new_at, &prefix[at_end..], suffix);
    }
    block.to_string()
}

/// Replace the `(fp_text reference "OLD" ...)` value in a footprint block.
pub fn replace_reference(block: &str, new_ref: &str) -> String {
    replace_fp_text(block, "reference", new_ref)
}

/// Replace the `(fp_text value "OLD" ...)` value in a footprint block.
pub fn replace_value(block: &str, new_val: &str) -> String {
    replace_fp_text(block, "value", new_val)
}

/// Replace the `(tstamp UUID)` in a footprint block.
pub fn replace_tstamp(block: &str, tstamp: &str) -> String {
    if let Some(pos) = block.find("(tstamp ") {
        let rel_close = block[pos..].find(')').unwrap_or(0);
        let end = pos + rel_close + 1;
        return format!("{}(tstamp {}){}", &block[..pos], tstamp, &block[end..]);
    }
    block.to_string()
}

/// Replace the library:name in `(footprint "lib:name" ...`.
pub fn replace_fp_name(block: &str, new_name: &str) -> String {
    let prefix = "(footprint \"";
    if let Some(pos) = block.find(prefix) {
        let name_start = pos + prefix.len();
        if let Some(end_rel) = block[name_start..].find('"') {
            return format!("{}{}{}", &block[..name_start], new_name, &block[name_start + end_rel..]);
        }
    }
    block.to_string()
}

fn replace_fp_text(block: &str, kind: &str, new_val: &str) -> String {
    // KiCad 6 format: (fp_text reference "REF**" ...)
    let marker6 = format!("fp_text {} \"", kind);
    if let Some(pos) = block.find(&marker6) {
        let val_start = pos + marker6.len();
        if let Some(end_rel) = block[val_start..].find('"') {
            return format!("{}{}{}", &block[..val_start], new_val, &block[val_start + end_rel..]);
        }
    }
    // KiCad 7+ format: (property "Reference" "REF**" ...) or (property "Value" "..." ...)
    // kind is "reference" or "value" — map to property key with capital first letter
    let prop_key = match kind {
        "reference" => "Reference",
        "value"     => "Value",
        other       => other,
    };
    let marker7 = format!("(property \"{}\" \"", prop_key);
    if let Some(pos) = block.find(&marker7) {
        let val_start = pos + marker7.len();
        if let Some(end_rel) = block[val_start..].find('"') {
            return format!("{}{}{}", &block[..val_start], new_val, &block[val_start + end_rel..]);
        }
    }
    block.to_string()
}

// ---------------------------------------------------------------------------
// .kicad_mod → PCB footprint conversion
// ---------------------------------------------------------------------------

/// Convert a `.kicad_mod` footprint definition into a PCB-embedded footprint block.
///
/// Strips `(version ...)`, `(generator ...)`, and `(layer ...)` from the mod body
/// (the layer moves to the header line), then injects the PCB-specific fields.
pub fn kicad_mod_to_pcb_footprint(
    mod_content: &str,
    library: &str,
    fp_name: &str,
    reference: &str,
    value: &str,
    x: f64,
    y: f64,
    rot: f64,
    tstamp: &str,
) -> String {
    // Drop the first line (contains "version" and "generator" keywords)
    let first_newline = mod_content.find('\n').unwrap_or(mod_content.len());
    let raw_body = &mod_content[first_newline..];

    // Strip the trailing ")" of the root footprint node
    let body = raw_body.trim_end().strip_suffix(')').unwrap_or(raw_body.trim_end());

    // Remove "(layer ...)" lines — the layer moves to the header
    // Add 2 extra spaces to each non-empty line (kicad_mod uses 2-space indent;
    // PCB footprint children conventionally use 4-space indent).
    let body: String = body
        .lines()
        .filter(|line| !line.trim_start().starts_with("(layer "))
        .map(|line| {
            if line.is_empty() { String::new() } else { format!("  {}", line) }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Substitute placeholder reference/value in fp_text nodes
    let body = replace_reference(&body, reference);
    let body = replace_value(&body, value);

    format!(
        "  (footprint \"{}:{}\" (layer \"F.Cu\")\n    (tstamp {})\n    {}\n{}\n  )",
        library, fp_name, tstamp, fmt_at(x, y, rot),
        body.trim_start_matches('\n')
    )
}

// ---------------------------------------------------------------------------
// Multi-component removal
// ---------------------------------------------------------------------------

/// Remove footprint blocks for the given references and return the modified content.
///
/// Ranges are processed in reverse order so earlier byte offsets stay valid.
pub fn remove_footprints(content: &str, refs: &[&str]) -> String {
    let blocks = find_footprint_blocks(content);

    let mut ranges: Vec<Range<usize>> = refs
        .iter()
        .filter_map(|r| blocks.get(*r).cloned())
        .collect();

    // Descending so removal doesn't shift earlier offsets
    ranges.sort_by(|a, b| b.start.cmp(&a.start));

    let mut result = content.to_string();
    for range in ranges {
        // Eat a trailing newline if present
        let end = if result.as_bytes().get(range.end) == Some(&b'\n') {
            range.end + 1
        } else {
            range.end
        };
        result.drain(range.start..end);
    }
    result
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

fn fmt_at(x: f64, y: f64, rot: f64) -> String {
    if (rot % 360.0).abs() < 0.001 {
        format!("(at {} {})", x, y)
    } else {
        format!("(at {} {} {})", x, y, rot)
    }
}

/// Generate a pseudo-tstamp from the current wall-clock time.
/// Not a proper UUID but fits the `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` format.
pub fn new_tstamp() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (ts >> 64) as u32,
        (ts >> 48) as u16,
        (ts >> 32) as u16,
        (ts >> 16) as u16,
        ts as u64 & 0xffff_ffff_ffff
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MINI_PCB: &str = r#"(kicad_pcb
  (footprint "Lib:FP_A" (layer "F.Cu")
    (tstamp aaaa-0000)
    (at 10 20 90)
    (fp_text reference "U1" (at 0 0) (layer "F.SilkS"))
    (fp_text value "IC1" (at 0 2) (layer "F.Fab"))
    (pad "1" thru_hole circle (at 0 0) (size 1.7 1.7) (drill 1.0))
  )
  (footprint "Lib:FP_B" (layer "F.Cu")
    (tstamp bbbb-0001)
    (at 50 60)
    (fp_text reference "C1" (at 0 0) (layer "F.SilkS"))
    (fp_text value "1u" (at 0 2) (layer "F.Fab"))
    (pad "1" thru_hole circle (at 0 0) (size 1.7 1.7) (drill 1.0))
  )
)"#;

    #[test]
    fn find_blocks_finds_all_refs() {
        let blocks = find_footprint_blocks(MINI_PCB);
        assert!(blocks.contains_key("U1"), "U1 not found");
        assert!(blocks.contains_key("C1"), "C1 not found");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn extract_at_ignores_pad_at() {
        let blocks = find_footprint_blocks(MINI_PCB);
        let range = blocks["U1"].clone();
        let block = &MINI_PCB[range];
        let (x, y, rot) = extract_at(block).unwrap();
        assert_eq!((x, y, rot), (10.0, 20.0, 90.0));
    }

    #[test]
    fn extract_at_defaults_rotation_to_zero() {
        let blocks = find_footprint_blocks(MINI_PCB);
        let range = blocks["C1"].clone();
        let block = &MINI_PCB[range];
        let (_, _, rot) = extract_at(block).unwrap();
        assert_eq!(rot, 0.0);
    }

    #[test]
    fn replace_at_round_trips() {
        let blocks = find_footprint_blocks(MINI_PCB);
        let range = blocks["U1"].clone();
        let block = &MINI_PCB[range];
        let replaced = replace_at(block, 99.5, 42.0, -90.0);
        let (x, y, rot) = extract_at(&replaced).unwrap();
        assert_eq!((x, y, rot), (99.5, 42.0, -90.0));
    }

    #[test]
    fn remove_footprints_removes_correct_block() {
        let modified = remove_footprints(MINI_PCB, &["C1"]);
        let blocks = find_footprint_blocks(&modified);
        assert!(blocks.contains_key("U1"));
        assert!(!blocks.contains_key("C1"));
    }

    #[test]
    fn sequential_replacements_both_survive() {
        // Simulate replace_footprint called twice: first U1, then C1.
        // After both, both new footprints must be present.
        let new_fp = |ref_: &str, fp: &str| -> String {
            format!(
                "  (footprint \"NewLib:{fp}\" (layer \"F.Cu\")\n    (tstamp zzzz-9999)\n    (at 0 0)\n    (fp_text reference \"{ref_}\" (at 0 0) (layer \"F.SilkS\"))\n    (fp_text value \"NEW\" (at 0 2) (layer \"F.Fab\"))\n  )",
                fp = fp, ref_ = ref_
            )
        };

        // Simulate the replace_footprint splice: trim trailing indent before inserting new block.
        let splice = |content: &str, range: std::ops::Range<usize>, block: String| -> String {
            let prefix_end = content[..range.start].trim_end_matches(|c: char| c == ' ' || c == '\t').len();
            format!("{}{}{}", &content[..prefix_end], block, &content[range.end..])
        };

        // Replace U1
        let blocks = find_footprint_blocks(MINI_PCB);
        let range = blocks["U1"].clone();
        let after_u1 = splice(MINI_PCB, range, new_fp("U1", "FP_NEW_A"));

        // Replace C1 in the already-modified content
        let blocks2 = find_footprint_blocks(&after_u1);
        assert!(blocks2.contains_key("U1"), "U1 missing after first replace");
        assert!(blocks2.contains_key("C1"), "C1 missing before second replace");
        let range2 = blocks2["C1"].clone();
        let after_both = splice(&after_u1, range2, new_fp("C1", "FP_NEW_B"));

        let blocks3 = find_footprint_blocks(&after_both);
        assert!(blocks3.contains_key("U1"), "U1 disappeared after second replace");
        assert!(blocks3.contains_key("C1"), "C1 disappeared after second replace");
        assert_eq!(blocks3.len(), 2);
    }

    #[test]
    fn replace_reference_and_value() {
        let blocks = find_footprint_blocks(MINI_PCB);
        let range = blocks["U1"].clone();
        let block = &MINI_PCB[range];
        let b = replace_reference(block, "U99");
        let b = replace_value(&b, "ATmega");
        assert_eq!(fp_text_value(&b, "reference").as_deref(), Some("U99"));
        assert_eq!(fp_text_value(&b, "value").as_deref(), Some("ATmega"));
    }
}
