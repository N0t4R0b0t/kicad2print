# Security Policy

## Supported versions

Only the latest release and the current `master` snapshot receive security fixes.

| Version | Supported |
|---|---|
| Latest release | ✅ |
| Older releases | ❌ |

## Reporting a vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report security issues privately via
[GitHub's private vulnerability reporting](https://github.com/N0t4R0b0t/kicad2print/security/advisories/new).

Include:

- A description of the vulnerability and its potential impact
- Steps to reproduce or a proof-of-concept (if safe to share)
- Any suggested mitigations you have in mind

You can expect an acknowledgement within **72 hours** and a resolution timeline
within **14 days** for confirmed vulnerabilities.

## Scope

kicad2print is a local CLI tool that reads `.kicad_pcb` files from disk and writes
STL/HTML output. It does not run a network server or handle untrusted remote input
in normal operation. The MCP server mode (`--mcp`) communicates only over stdio
with a locally running Claude Desktop process.

Relevant attack surfaces:

- **Malicious `.kicad_pcb` files** — path traversal, parser panics, or excessive
  resource consumption triggered by crafted input.
- **MCP server** — unexpected input via the MCP stdio channel.
- **Build/release pipeline** — supply-chain issues in dependencies.
