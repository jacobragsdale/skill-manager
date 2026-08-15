# Target adapter contract

This reference defines the acceptance bar for a built-in target adapter. Adapters are pure planners: they may inspect a validated component and profile, but they may not write files, start processes, change the ledger, download code, or suppress errors.

## Stable targets and pinned dialects

| Target             | Stable ID        | User-scope dialect       | Skills             | MCP                          | Instructions                                       |
| ------------------ | ---------------- | ------------------------ | ------------------ | ---------------------------- | -------------------------------------------------- |
| Cursor             | `cursor`         | `cursor-2026-08`         | `~/.agents/skills` | `~/.cursor/mcp.json`         | Unsupported: no documented writable user-rule file |
| Claude Code        | `claude-code`    | `claude-code-2026-08`    | `~/.claude/skills` | `~/.claude.json`             | Managed block in `~/.claude/CLAUDE.md`             |
| Codex              | `codex`          | `codex-2026-08`          | `~/.agents/skills` | `~/.codex/config.toml`       | Managed block in `~/.codex/AGENTS.md`              |
| OpenCode           | `opencode`       | `opencode-2026-08`       | `~/.agents/skills` | user `opencode.jsonc`        | Managed block in user `AGENTS.md`                  |
| Grok Build         | `grok-build`     | `grok-build-2026-08`     | `~/.grok/skills`   | `~/.grok/config.toml`        | Unsupported at user scope                          |
| GitHub Copilot CLI | `github-copilot` | `github-copilot-2026-08` | `~/.agents/skills` | `~/.copilot/mcp-config.json` | Unsupported at user scope                          |

Cursor and GitHub Copilot additionally preserve a portable Agent Plugin package in their documented local/direct plugin location. Other targets receive its supported skill and MCP components.

`opencode-2026-08` pins the stable `opencode.jsonc` contract with a root `mcp` object. The separate beta/v2 `mcp.servers` contract requires a new dialect and is not selected implicitly.

## Capability result

Every component/target pair returns one of `native`, `losslessTranslation`, `lossyTranslation`, `unsupported`, or `blocked`. Lossy results list each lost semantic. Unsupported and blocked results include an actionable reason and are never collapsed into success.

An unknown dialect may use only the documented shared `~/.agents/skills` projection. Shared-config entries, target-specific skill locations, instructions, and native plugin paths remain blocked until that dialect is explicitly supported.

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

- Cursor: reload the window, then inspect the Plugins and MCP settings surfaces.
- Claude Code: use its MCP listing command and inspect the effective user instructions.
- Codex: inspect configured MCP servers and start a fresh session for instruction/skill discovery.
- OpenCode and Grok Build: use the client configuration/MCP inspection surface for the pinned release.
- GitHub Copilot CLI: use `copilot plugin list` for direct plugins and its MCP inspection surface.

Run these checks in a disposable home. Record the target version, operating system, reload boundary, command/output, and whether the evidence is file-only or observed runtime discovery.
