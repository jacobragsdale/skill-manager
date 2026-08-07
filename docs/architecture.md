# Backend architecture

Skill Manager keeps one Rust crate while separating acquisition, catalog
normalization, installation, application orchestration, and desktop transport.
This boundary keeps repository data from directly controlling filesystem
changes and leaves room for optional metadata without changing the command
surface.

## Module ownership

### `lib.rs`

The crate root wires the private modules and constructs the Tauri runtime. It
registers plugins and IPC commands, installs tray and window lifecycle hooks,
and starts the scheduled synchronization task.

### `domain`

`domain` contains Tauri-free source, catalog, status, ownership, and destination
models. A destination is an approved anchor plus a validated relative path.
Today the only anchor is the current user's home directory, and a skill named
`<name>` resolves to `.agents/skills/<name>` beneath it.

Absolute paths, parent traversal, and multi-component skill names are rejected
before resolution. Destination construction is centralized here so additional
anchors do not spread path rules through commands or application workflows.

### `catalog`

`catalog` normalizes the conventional `skills/<name>/SKILL.md` layout. It owns
skill-name and frontmatter parsing, directory digests, portable-path checks, and
per-entry errors. It produces a catalog of independently installable skills;
`bundles/` and other repository metadata are outside this input contract.

### `sources`

`sources` owns source identity and configuration, Git and GitHub retrieval,
commit-pinned cache metadata, sparse staging of `skills/`, activation, legacy
cache migration, and validated-cache fallback. Network retrieval remains
asynchronous. Filesystem-heavy staging and validation are dispatched through
the application's blocking-work boundary.

### `install`

`install` is the filesystem ownership boundary. It resolves targets, reads and
writes `.skill-manager-managed`, inspects status, stages copies, adopts exact
unmanaged directories, performs backup-first replacement, updates managed
content, and protects uninstall.

Source-wide planning produces ordered changes containing anchored destinations
without mutating the filesystem. Execution rejects a plan with attention items
before applying any change, then records per-skill successes and failures so a
retry is safe. Individual commands use the same install subsystem and ownership
checks.

### `application`

`application` owns cached loading, synchronization, automatic updates, source
lifecycle workflows, application-state projection, and blocking-work dispatch.
It serializes synchronization with the sync lock and takes the catalog lock
inside that workflow when cache or installation state is read or changed.

### `ipc`

`ipc` owns the serialized frontend contracts and thin Tauri adapters. Adapters
only unwrap managed runtime state and call the application service. Domain and
persistence types do not become IPC contracts by accident.

The existing command names remain stable: individual skill and source commands
plus source-only `plan_install_all`, `install_all`, `plan_uninstall_all`, and
`uninstall_all`.

## Extension boundaries

### Optional source manifest

A future parser may translate an optional source manifest into the same
normalized catalog that the conventional `skills/` reader produces. Source
acquisition, application commands, planning, and execution should not need a
second path. Repositories without a manifest must retain today's defaults.

### Additional destinations

Future metadata may select only anchors explicitly approved in local code. It
must still provide a validated relative path, and all resolution must continue
through `domain::Destination`.

The embedded ownership marker remains the current persistence mechanism. A
future multi-destination ledger may supplement it behind the install subsystem;
callers should not depend on either storage format.

### Executable plan steps and trust

The plan model can gain executable steps later, but no scripts or trust state
exist now. Authorization must come from locally persisted trust bound to the
stable source identity. Repository metadata cannot authorize itself, and a
source URL or manifest declaration alone must never permit execution.
