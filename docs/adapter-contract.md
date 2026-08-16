# Target adapter contract

This reference defines the acceptance bar for a built-in target adapter. Adapters are pure planners: they may inspect a validated component and profile, but they may not write files, start processes, change the ledger, download code, or suppress errors.

## Stable targets and pinned dialects

| Target         | Stable ID        | User-scope dialect       | Skills             | MCP                          |
| -------------- | ---------------- | ------------------------ | ------------------ | ---------------------------- |
| Cursor         | `cursor`         | `cursor-2026-08`         | `~/.agents/skills` | `~/.cursor/mcp.json`         |
| Claude Code    | `claude-code`    | `claude-code-2026-08`    | `~/.claude/skills` | `~/.claude.json`             |
| Codex          | `codex`          | `codex-2026-08`          | `~/.agents/skills` | `~/.codex/config.toml`       |
| OpenCode       | `opencode`       | `opencode-2026-08`       | `~/.agents/skills` | user `opencode.jsonc`        |
| Grok Build     | `grok-build`     | `grok-build-2026-08`     | `~/.agents/skills` | `~/.grok/config.toml`        |
| GitHub Copilot | `github-copilot` | `github-copilot-2026-08` | `~/.agents/skills` | `~/.copilot/mcp-config.json` |

A v2 package may contain several skill and MCP components. Agent Plugins does not install native `agent-plugin@1.0.0` package trees or always-on instruction files.

`opencode-2026-08` pins the stable `opencode.jsonc` contract with a root `mcp` object. The separate beta/v2 `mcp.servers` contract requires a new dialect and is not selected implicitly.

## Capability result

Every component/target pair returns one of `native`, `losslessTranslation`, `lossyTranslation`, `unsupported`, or `blocked`. Lossy results list each lost semantic. Unsupported and blocked results include an actionable reason and are never collapsed into success.

Detection, not a user enable list, chooses which agents are configured. Skills for every detected agent except Claude Code share `~/.agents/skills`. Claude Code uses `~/.claude/skills`. Cursor and other compatibility scanners may also see the Claude folder.

An unknown dialect may use only the documented shared `~/.agents/skills` projection. Shared-config entries and target-specific skill locations remain blocked until that dialect is explicitly supported.

## Conformance checklist

An adapter is complete only when it has:

- stable target and dialect IDs;
- official documentation links and a last-verified date;
- advisory version detection with conservative behavior for unknown dialects;
- a capability result for every canonical component kind;
- pure desired-resource fixtures and coalescing coverage;
- documented user scope, naming, precedence, and reload semantics;
- transaction and drift coverage for every emitted resource type;
- UI copy for lossy, blocked, and unsupported mappings; and
- disposable runtime discovery evidence. Static file checks prove disk state, not that an agent loaded it.

## Registration checks

Runtime checks must inspect registration without intentionally starting source executables:

- Cursor: reload the window, then inspect the Skills and MCP settings surfaces.
- Claude Code: use its MCP listing command and inspect discovered user skills.
- Codex: inspect configured MCP servers and start a fresh session for skill discovery.
- OpenCode and Grok Build: use the client configuration/MCP inspection surface for the pinned release.
- GitHub Copilot: inspect skills in the IDE Copilot agent, or use Copilot CLI if it is installed.

Run these checks in a disposable home. Record the target version, operating system, reload boundary, command/output, and whether the evidence is file-only or observed runtime discovery.
