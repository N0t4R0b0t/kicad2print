# Contributing to kicad2print

Thank you for your interest in contributing! kicad2print turns KiCad PCB designs
into 3D-printable substrates, and every improvement — bug fix, new feature, or
documentation clarification — helps makers build real boards faster.

---

## Ways to contribute

| Type | Where |
|---|---|
| Bug reports | [GitHub Issues](https://github.com/N0t4R0b0t/kicad2print/issues) — use the **Bug report** template |
| Feature requests | [GitHub Issues](https://github.com/N0t4R0b0t/kicad2print/issues) — use the **Feature request** template |
| Code changes | Fork → branch → pull request (see below) |
| Documentation | Same PR flow — docs live in `docs/` and inline in `README.md` |
| Example boards | Add a self-contained folder under `examples/` |

---

## Development setup

**Prerequisites:** Rust stable (≥ 1.78) via [rustup](https://rustup.rs).

```bash
git clone https://github.com/N0t4R0b0t/kicad2print.git
cd kicad2print
cargo build
cargo test
```

Run against a real board:

```bash
cargo run -- examples/ps2-serial-mouse-adapter/ps2-serial-mouse-adapter.kicad_pcb
```

---

## Submitting a pull request

1. **Fork** the repository and create a branch off `master`:
   ```bash
   git checkout -b feat/my-improvement
   ```

2. **Make your changes.** Keep commits focused and use
   [Conventional Commits](https://www.conventionalcommits.org/) prefixes:
   `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.

3. **Test your changes:**
   ```bash
   cargo test
   cargo clippy -- -D warnings
   cargo fmt --check
   ```

4. **Open a pull request** against `master`. Fill in the PR template — especially
   the "How to test" section so reviewers can verify the change quickly.

---

## Code style

- Run `cargo fmt` before committing.
- No new `clippy` warnings — fix them or add a justified `#[allow(...)]` with a comment.
- Keep public items documented with `///` doc comments.

---

## Adding a new example board

1. Create `examples/<board-name>/` with the `.kicad_pcb` file and a brief `README.md`.
2. Run kicad2print against it and commit the generated STL/HTML output.
3. Reference it in the top-level `README.md` examples table.

---

## Licence note

By submitting a contribution you agree that your work will be released under the
project's [AGPL-3.0 licence](LICENSE).
