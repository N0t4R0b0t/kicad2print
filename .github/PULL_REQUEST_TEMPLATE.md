## Summary

<!-- What does this PR do? One or two sentences. -->

## Motivation

<!-- Why is this change needed? Link to a related issue if one exists: Closes #123 -->

## Changes

<!-- Bullet list of what was added, changed, or removed. -->

- 

## How to test

<!-- Steps a reviewer can follow to verify the change works. Include the exact
     command, flag, or board file to use. -->

```bash
cargo build
kicad2print examples/ps2-serial-mouse-adapter/ps2-serial-mouse-adapter.kicad_pcb
```

## Checklist

- [ ] `cargo test` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] Relevant documentation updated (README, docs/, inline comments)
- [ ] New public items have `///` doc comments
