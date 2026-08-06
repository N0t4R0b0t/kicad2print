# Agent defs

Drop subagent definitions here as `.md` files. Each needs YAML frontmatter with
at least `name` and `description`; once the plugin is installed, the agent
appears in the `@`-mention list as `kicad2print:<name>`.

To scope an agent to only this plugin's MCP tools, add:

```yaml
tools: ["mcp__plugin_kicad2print_kicad2print__*"]
```

See https://code.claude.com/docs/en/plugins-reference for the full agent
frontmatter schema (`model`, `effort`, `maxTurns`, `disallowedTools`, `skills`,
`memory`, `background`, `isolation`).
