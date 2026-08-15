# Target-native extension evaluation

This explanation records the Phase 6 decision for extension families that can execute automatically or extend an agent runtime. None is a portable manifest v2 component.

## Decision matrix

| Family                       | Execution lifecycle                                 | Main permissions and risks                                                           | Ownership/uninstall requirement                                                               | Decision                                              |
| ---------------------------- | --------------------------------------------------- | ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| Hooks                        | Runs automatically on target events                 | Arbitrary process execution, source/worktree access, environment leakage             | Own exact hook entry and payload; prove target stops invoking it before deletion              | Deferred; separate Tier 4 workflow required           |
| In-process plugins           | Loaded into the target process                      | Full target-process privileges, API instability, dependency execution                | Versioned target package identity, cache/data separation, reload and disable proof            | Deferred; never translate across targets              |
| Agents/subagents             | Invoked by model/user and may have tools            | Delegated tool authority, prompt/instruction precedence, possible autonomous actions | Own exact target definition and expose effective tools/model/instructions                     | Deferred until each target has a public stable schema |
| LSP servers                  | Long-lived process started for matching files       | Process/network access, workspace contents, executable acquisition                   | Own registration and binaries separately; unregister before binary cleanup                    | Deferred; requires Tier 4 process disclosure          |
| Monitors/background services | Starts or persists without an immediate user action | Persistence, resource consumption, network/process access                            | Transaction must include OS/target registration, stop verification, and data-retention choice | Out of scope for portable sources                     |

## Admission requirements

Supporting one family for one target requires a target-specific ADR containing:

- stable target ID, pinned dialect/version range, and authoritative format reference;
- exact activation and reload lifecycle;
- effective process, filesystem, network, secret, and tool permissions;
- signing, provenance, update, and dependency behavior;
- explicit Tier 4 approval copy;
- desired resource types for registration, payload, cache, and data;
- rollback and crash-recovery behavior;
- safe disable-before-delete uninstall proof; and
- disposable runtime evidence using the target's own inspection surface.

Unknown or newer dialects are read-only. Source-published adapter code, hooks, or dependencies are never executed to discover compatibility.

## Architecture consequence

The current resource graph can represent owned paths and structured registrations, but that is insufficient by itself: activation and deactivation are runtime state transitions. A future implementation must add an explicit target-qualified lifecycle resource and verifier before any of these families can be enabled. Until then, adapters return `unsupported` or `blocked` with the required action.
