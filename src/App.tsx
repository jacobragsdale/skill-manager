import { startTransition, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { JSX, SyntheticEvent } from "react";
import { Badge, Button, Callout, Card, Code, Dialog, Heading, Spinner, Text, TextField } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { z } from "zod";
import "./App.css";

const AGENT_SKILLS_URL = "https://agentskills.io/specification";
const SCHEDULED_SYNC_EVENT = "scheduled-sync";
const OPERATION_OUTPUT_EVENT = "operation-output";
const MAX_VISIBLE_LOG_CHARACTERS = 100_000;

const itemStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict", "sourceConflict", "incomplete", "unsupported"]);
const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
const destinationAnchorSchema = z.enum(["home", "config", "data", "localData", "cache"]);
const catalogErrorSchema = z.strictObject({ path: z.string().min(1), message: z.string().min(1) }).readonly();
const actionSchema = z.strictObject({ id: z.string().min(1), localId: z.string().min(1), name: z.string().min(1), description: z.string().min(1), supported: z.boolean() }).readonly();
const agentSkillSchema = z
  .strictObject({
    localName: z.string().min(1),
    license: z.string().min(1).nullable(),
    compatibility: z.string().min(1).nullable(),
    metadata: z.record(z.string(), z.string()).readonly(),
    allowedTools: z.string().min(1).nullable(),
    manualOnly: z.boolean()
  })
  .readonly();
const destinationSchema = z.strictObject({ anchor: destinationAnchorSchema, path: z.string().min(1) }).readonly();
const itemSchema = z
  .strictObject({
    id: z.string().min(3),
    localId: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    sourceName: z.string().min(1),
    sourceUrl: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    kind: z.string().min(1),
    materializedSkillName: z.string().min(1).nullable(),
    agentSkill: agentSkillSchema.nullable(),
    destinations: z.array(destinationSchema).readonly(),
    status: itemStatusSchema,
    executable: z.boolean(),
    actions: z.array(actionSchema).readonly()
  })
  .readonly();
const sourceSchema = z
  .strictObject({
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    builtIn: z.boolean(),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    commit: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    catalogErrors: z.array(catalogErrorSchema).readonly(),
    executable: z.boolean(),
    trusted: z.boolean(),
    trustRequired: z.boolean(),
    actions: z.array(actionSchema).readonly()
  })
  .readonly();
const itemReferenceSchema = z.strictObject({ id: z.string().min(1), sourceId: z.string().min(2), localId: z.string().min(1) }).readonly();
const itemFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const autoUpdateReportSchema = z
  .strictObject({
    updatedItems: z.array(itemReferenceSchema).readonly(),
    skippedUntrustedItems: z.array(itemReferenceSchema).readonly(),
    migrationAttention: z.array(itemFailureSchema).readonly(),
    failedItems: z.array(itemFailureSchema).readonly()
  })
  .readonly();
const appStateSchema = z
  .strictObject({ checkedAtEpochSeconds: z.number().int().nonnegative(), autoUpdateReport: autoUpdateReportSchema, sources: z.array(sourceSchema).readonly(), items: z.array(itemSchema).readonly() })
  .readonly();
const preparedSourceSchema = z
  .strictObject({
    token: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    commit: z.string().min(1),
    executable: z.boolean(),
    itemCount: z.number().int().nonnegative()
  })
  .readonly();
const executionLogSchema = z.strictObject({ stdoutPath: z.string().min(1), stderrPath: z.string().min(1), stepId: z.string().min(1), success: z.boolean() }).readonly();
const operationOutcomeSchema = z.strictObject({ incomplete: z.boolean(), logs: z.array(executionLogSchema).readonly(), backupPaths: z.array(z.string().min(1)).readonly() }).readonly();
const bulkPlanEntrySchema = z.strictObject({ id: z.string().min(1), localId: z.string().min(1), status: itemStatusSchema, willRun: z.boolean() }).readonly();
const bulkPlanSchema = z.strictObject({ sourceId: z.string().min(2), uninstall: z.boolean(), entries: z.array(bulkPlanEntrySchema).readonly() }).readonly();
const bulkFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const bulkResultSchema = z.strictObject({ completed: z.array(z.string().min(1)).readonly(), failures: z.array(bulkFailureSchema).readonly() }).readonly();
const removalPathSchema = z.strictObject({ path: z.string().min(1), modified: z.boolean() }).readonly();
const removalItemSchema = z.strictObject({ id: z.string().min(1), paths: z.array(removalPathSchema).readonly() }).readonly();
const sourceRemovalPlanSchema = z.strictObject({ sourceId: z.string().min(2), executableCleanup: z.boolean(), items: z.array(removalItemSchema).readonly() }).readonly();
const scheduledSyncSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("updated"), state: appStateSchema }).readonly(),
  z.strictObject({ kind: z.literal("failed"), message: z.string().min(1) }).readonly()
]);
const operationOutputSchema = z.strictObject({ operationId: z.string().min(1), stream: z.enum(["stdout", "stderr"]), text: z.string() }).readonly();
const cachedStateSchema = appStateSchema.nullable();
const unitSchema = z.null();

type AppState = z.infer<typeof appStateSchema>;
type CatalogItem = z.infer<typeof itemSchema>;
type ItemStatus = z.infer<typeof itemStatusSchema>;
type OperationOutcome = z.infer<typeof operationOutcomeSchema>;
type PreparedSource = z.infer<typeof preparedSourceSchema>;
type SourceState = z.infer<typeof sourceSchema>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

async function invokeParsed<T>(command: string, schema: z.ZodType<T>, args?: Record<string, unknown>): Promise<T> {
  const payload = args === undefined ? await invoke<unknown>(command) : await invoke<unknown>(command, args);
  return schema.parse(payload);
}

function errorText(reason: unknown): string {
  return reason instanceof z.ZodError ? `Skill Manager returned invalid data: ${z.prettifyError(reason)}` : String(reason);
}

function operationId(label: string): string {
  return `${label}-${String(Date.now())}-${crypto.randomUUID()}`;
}

function statusLabel(status: ItemStatus): string {
  switch (status) {
    case "available":
      return "Available";
    case "installed":
      return "Installed";
    case "updateAvailable":
      return "Update Available";
    case "removed":
      return "Removed Upstream";
    case "modified":
      return "Local Changes";
    case "conflict":
      return "Unmanaged Conflict";
    case "sourceConflict":
      return "Source Conflict";
    case "incomplete":
      return "Incomplete";
    case "unsupported":
      return "Unsupported";
  }
}

function statusColor(status: ItemStatus): AccentColor {
  switch (status) {
    case "installed":
      return "green";
    case "updateAvailable":
      return "blue";
    case "available":
    case "unsupported":
      return "gray";
    case "removed":
    case "conflict":
    case "sourceConflict":
      return "amber";
    case "modified":
    case "incomplete":
      return "red";
  }
}

function primaryActionLabel(status: ItemStatus): string {
  switch (status) {
    case "available":
      return "Install";
    case "updateAvailable":
      return "Update";
    case "installed":
    case "removed":
      return "Uninstall";
    case "conflict":
      return "Manage…";
    case "incomplete":
      return "Retry";
    case "modified":
      return "Protected";
    case "sourceConflict":
      return "Owned Elsewhere";
    case "unsupported":
      return "Unsupported";
  }
}

function canChangeItem(status: ItemStatus): boolean {
  return !matchesProtectedStatus(status);
}

function matchesProtectedStatus(status: ItemStatus): boolean {
  return status === "modified" || status === "sourceConflict" || status === "unsupported";
}

function reportMessage(state: AppState): string | null {
  const report = state.autoUpdateReport;
  const parts: string[] = [];
  if (report.updatedItems.length > 0) {
    parts.push(`Updated ${report.updatedItems.map((item) => item.id).join(", ")}.`);
  }
  if (report.skippedUntrustedItems.length > 0) {
    parts.push(`Skipped untrusted update hooks for ${report.skippedUntrustedItems.map((item) => item.id).join(", ")}.`);
  }
  if (report.migrationAttention.length > 0) {
    parts.push(`Namespace migration needs attention: ${report.migrationAttention.map((item) => `${item.id}: ${item.message}`).join("; ")}.`);
  }
  if (report.failedItems.length > 0) {
    parts.push(`Background work failed: ${report.failedItems.map((item) => `${item.id}: ${item.message}`).join("; ")}.`);
  }
  return parts.length === 0 ? null : parts.join(" ");
}

function Notice({ error, state, onDismiss }: Readonly<{ error: string | null; state: AppState | null; onDismiss: () => void }>): JSX.Element | null {
  const report = state === null ? null : reportMessage(state);
  if (error === null && report === null) {
    return null;
  }
  const isError = error !== null;
  return (
    <Callout.Root className="app-callout notice" color={isError ? "red" : "amber"} role={isError ? "alert" : "status"}>
      <div className="callout-content">
        <Callout.Text>{error ?? report}</Callout.Text>
        {isError ? (
          <Button className="callout-action" size="1" variant="soft" onClick={onDismiss}>
            Dismiss
          </Button>
        ) : null}
      </div>
    </Callout.Root>
  );
}

function ExecutionPanel({ text, outcome, onRevealError }: Readonly<{ text: string; outcome: OperationOutcome | null; onRevealError: (message: string) => void }>): JSX.Element | null {
  if (text.length === 0 && outcome === null) {
    return null;
  }
  return (
    <Card className="execution-panel">
      <div className="execution-heading">
        <Heading as="h3" size="2">
          Execution log
        </Heading>
        {outcome?.logs.map((log) => (
          <Button
            key={`${log.stepId}-${log.stdoutPath}`}
            size="1"
            variant="ghost"
            onClick={() => {
              revealItemInDir(log.stdoutPath).catch((reason: unknown) => {
                onRevealError(errorText(reason));
              });
            }}
          >
            Reveal {log.stepId}
          </Button>
        ))}
      </div>
      {text.length > 0 ? <pre className="execution-output">{text}</pre> : null}
      {outcome !== null && outcome.backupPaths.length > 0 ? (
        <Text as="p" color="amber" size="1">
          Backup retained at {outcome.backupPaths.join(", ")}
        </Text>
      ) : null}
    </Card>
  );
}

function DestinationList({ item, onError }: Readonly<{ item: CatalogItem; onError: (message: string) => void }>): JSX.Element {
  return (
    <div className="destination-list">
      {item.destinations.map((destination) => (
        <Button
          key={`${destination.anchor}:${destination.path}`}
          size="1"
          variant="ghost"
          onClick={() => {
            revealItemInDir(destination.path).catch((reason: unknown) => {
              onError(errorText(reason));
            });
          }}
        >
          Reveal {destination.anchor}: {destination.path}
        </Button>
      ))}
    </div>
  );
}

function ItemCard({
  item,
  source,
  busy,
  onChange,
  onAction,
  onError
}: Readonly<{
  item: CatalogItem;
  source: SourceState;
  busy: boolean;
  onChange: (item: CatalogItem) => Promise<void>;
  onAction: (item: CatalogItem, actionId: string) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const title = item.materializedSkillName ?? item.name;
  const executableBlocked = item.executable && !source.trusted;
  return (
    <Card className="skill-card item-card">
      <div className="skill-copy">
        <div className="skill-title-row">
          <Heading as="h4" size="3">
            {title}
          </Heading>
          <Badge color={statusColor(item.status)}>{statusLabel(item.status)}</Badge>
          {item.agentSkill?.manualOnly === true ? <Badge color="amber">Manual Only</Badge> : null}
          {item.executable ? <Badge color={source.trusted ? "green" : "red"}>{source.trusted ? "Executable · Trusted" : "Executable · Blocked"}</Badge> : null}
        </div>
        <Text as="p" color="gray" size="2">
          {item.description}
        </Text>
        <Code className="canonical-id" color="gray" size="1" variant="ghost">
          {item.id}
        </Code>
        <details className="item-details">
          <summary>Details and destinations</summary>
          <dl>
            <dt>Kind</dt>
            <dd>{item.kind}</dd>
            <dt>Local ID</dt>
            <dd>{item.localId}</dd>
            {item.agentSkill === null ? null : (
              <>
                <dt>Local skill name</dt>
                <dd>{item.agentSkill.localName}</dd>
                <dt>License</dt>
                <dd>{item.agentSkill.license ?? "Not declared"}</dd>
                <dt>Compatibility</dt>
                <dd>{item.agentSkill.compatibility ?? "Not declared"}</dd>
                <dt>Allowed tools</dt>
                <dd>{item.agentSkill.allowedTools ?? "Not declared"}</dd>
              </>
            )}
          </dl>
          <DestinationList item={item} onError={onError} />
        </details>
      </div>
      <div className="item-actions">
        {item.actions.map((action) => (
          <Button
            key={action.id}
            size="1"
            variant="surface"
            disabled={busy || !source.trusted || !action.supported}
            title={action.description}
            onClick={() => {
              onAction(item, action.localId).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            {action.name}
            {action.supported ? "" : " · Unsupported"}
          </Button>
        ))}
        <Button
          className="skill-action skill-action-primary"
          disabled={busy || !canChangeItem(item.status) || executableBlocked}
          loading={busy}
          size="2"
          onClick={() => {
            onChange(item).catch((reason: unknown) => {
              onError(errorText(reason));
            });
          }}
        >
          {primaryActionLabel(item.status)}
        </Button>
      </div>
    </Card>
  );
}

function SourceGroup({
  source,
  items,
  busyId,
  onChange,
  onItemAction,
  onSourceAction,
  onBulk,
  onError
}: Readonly<{
  source: SourceState;
  items: readonly CatalogItem[];
  busyId: string | null;
  onChange: (item: CatalogItem) => Promise<void>;
  onItemAction: (item: CatalogItem, actionId: string) => Promise<void>;
  onSourceAction: (source: SourceState, actionId: string) => Promise<void>;
  onBulk: (source: SourceState, uninstall: boolean) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const allBusy = busyId !== null;
  return (
    <section className="source-group source-group-bordered">
      <div className="source-group-heading">
        <div className="source-group-copy">
          <div className="source-title-row">
            <Heading as="h3" size="4">
              {source.name}
            </Heading>
            <Badge color="blue">{source.sourceId}</Badge>
            {source.executable ? <Badge color={source.trusted ? "green" : "red"}>{source.trusted ? "Trusted" : "Trust Required"}</Badge> : null}
          </div>
          <Text as="p" color="gray" size="2">
            {source.description}
          </Text>
          <button
            className="repository-url-link"
            type="button"
            onClick={() => {
              openUrl(source.url).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            <Code className="source-url" color="gray" size="1" variant="ghost">
              {source.url}
            </Code>
          </button>
        </div>
        <div className="source-group-actions">
          {source.actions.map((action) => (
            <Button
              key={action.id}
              disabled={allBusy || !source.trusted || !action.supported}
              size="1"
              title={action.description}
              variant="surface"
              onClick={() => {
                onSourceAction(source, action.localId).catch((reason: unknown) => {
                  onError(errorText(reason));
                });
              }}
            >
              {action.name}
              {action.supported ? "" : " · Unsupported"}
            </Button>
          ))}
          <Button
            disabled={allBusy}
            size="1"
            variant="surface"
            onClick={() => {
              onBulk(source, false).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Install updates
          </Button>
          <Button
            color="red"
            disabled={allBusy}
            size="1"
            variant="surface"
            onClick={() => {
              onBulk(source, true).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Uninstall all
          </Button>
        </div>
      </div>
      {source.message === null ? null : (
        <Callout.Root className="source-callout" color="amber" role="status">
          <Callout.Text>{source.message}</Callout.Text>
        </Callout.Root>
      )}
      {source.catalogErrors.map((catalogError) => (
        <Callout.Root key={`${catalogError.path}:${catalogError.message}`} className="source-callout" color="red" role="alert">
          <Callout.Text>
            {catalogError.path}: {catalogError.message}
          </Callout.Text>
        </Callout.Root>
      ))}
      <div className="source-skill-list">
        {items.map((item) => (
          <ItemCard
            key={item.id}
            item={item}
            source={source}
            busy={busyId === item.id || busyId === source.sourceId || busyId === "bulk"}
            onChange={onChange}
            onAction={onItemAction}
            onError={onError}
          />
        ))}
        {items.length === 0 ? (
          <Card className="empty-source-card">
            <Text color="gray">This source currently publishes no valid items for this platform.</Text>
          </Card>
        ) : null}
      </div>
    </section>
  );
}

function SourcesDialog({
  open,
  state,
  url,
  busyId,
  onOpenChange,
  onUrlChange,
  onAdd,
  onAddDefault,
  onTrust,
  onRemove
}: Readonly<{
  open: boolean;
  state: AppState | null;
  url: string;
  busyId: string | null;
  onOpenChange: (open: boolean) => void;
  onUrlChange: (value: string) => void;
  onAdd: (event: SyntheticEvent<HTMLFormElement>) => Promise<void>;
  onAddDefault: () => Promise<void>;
  onTrust: (source: SourceState, trusted: boolean) => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
}>): JSX.Element {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Trigger>
        <Button className="sources-button" variant="surface">
          Sources
        </Button>
      </Dialog.Trigger>
      <Dialog.Content className="sources-dialog" maxWidth="720px">
        <Dialog.Title>Sources</Dialog.Title>
        <Dialog.Description>Namespaces are unique. Executable trust is bound to the repository URL and source key, never the namespace.</Dialog.Description>
        <div className="source-manager-list">
          {state?.sources.map((source) => (
            <Card key={source.sourceKey} className="source-item">
              <div className="source-item-topline">
                <div>
                  <Heading as="h3" size="3">
                    {source.name} <Badge color="blue">{source.sourceId}</Badge>
                  </Heading>
                  <Code className="source-item-url" color="gray" size="1" variant="ghost">
                    {source.url}
                  </Code>
                </div>
                <div className="dialog-actions">
                  {source.executable || source.trusted ? (
                    <Button
                      color={source.trusted ? "red" : "amber"}
                      disabled={busyId !== null}
                      size="1"
                      variant="surface"
                      onClick={() => {
                        onTrust(source, !source.trusted).catch(() => undefined);
                      }}
                    >
                      {source.trusted ? "Revoke trust" : "Grant trust…"}
                    </Button>
                  ) : null}
                  <Button
                    color="red"
                    disabled={busyId !== null}
                    size="1"
                    variant="soft"
                    onClick={() => {
                      onRemove(source).catch(() => undefined);
                    }}
                  >
                    Remove…
                  </Button>
                </div>
              </div>
              <Text className="source-item-meta" as="p" color="gray" size="1">
                sourceKey {source.sourceKey} · {source.status} · {source.commit?.slice(0, 12) ?? "no validated commit"}
              </Text>
            </Card>
          ))}
        </div>
        <form className="add-source-form" onSubmit={(event) => void onAdd(event)}>
          <TextField.Root
            value={url}
            placeholder="https://github.com/owner/repository"
            onChange={(event) => {
              onUrlChange(event.currentTarget.value);
            }}
          />
          <div className="add-source-controls">
            <Button type="submit" disabled={busyId !== null || url.trim().length === 0}>
              Prepare source
            </Button>
            {state?.sources.some((source) => source.builtIn) === false ? (
              <Button
                type="button"
                variant="surface"
                disabled={busyId !== null}
                onClick={() => {
                  onAddDefault().catch(() => undefined);
                }}
              >
                Add Skillbook
              </Button>
            ) : null}
          </div>
        </form>
        <Dialog.Close>
          <Button variant="soft">Close</Button>
        </Dialog.Close>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function AboutDialog(): JSX.Element {
  return (
    <Dialog.Root>
      <Dialog.Trigger>
        <Button className="about-button" variant="surface">
          About
        </Button>
      </Dialog.Trigger>
      <Dialog.Content className="about-dialog" maxWidth="620px">
        <Dialog.Title>Skill Manager</Dialog.Title>
        <Dialog.Description>Manifest-driven, source-namespaced Agent Skills and per-user configuration.</Dialog.Description>
        <div className="about-sections">
          <Text as="p" color="gray" size="2">
            Catalog IDs use <Code>source-id/local-id</Code>. Agent Skill directories and frontmatter use <Code>source-id-local-name</Code>.
          </Text>
          <Text as="p" color="gray" size="2">
            Hooks and actions run unsandboxed as your user only after repository-specific executable trust is granted.
          </Text>
          <Button
            variant="ghost"
            onClick={() => {
              openUrl(AGENT_SKILLS_URL).catch(() => undefined);
            }}
          >
            Agent Skills specification
          </Button>
        </div>
        <Dialog.Close>
          <Button variant="soft">Close</Button>
        </Dialog.Close>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function preparedSourcePrompt(prepared: PreparedSource): Readonly<{ text: string; title: string; kind: "info" | "warning"; okLabel: string }> {
  const summary = `${prepared.name} (${prepared.sourceId})\n${prepared.url}\nCommit ${prepared.commit.slice(0, 12)} · ${String(prepared.itemCount)} items`;
  if (!prepared.executable) {
    return { text: `${summary}\n\nAdd this declarative source?`, title: "Confirm source", kind: "info", okLabel: "Add Source" };
  }
  return {
    text: `${summary}\n\nThis source can run unsandboxed programs with your full filesystem, process, and network access. Trust also covers future changed code and scheduled background update hooks. Add and trust this source?`,
    title: "Trust executable source",
    kind: "warning",
    okLabel: "Add and Trust"
  };
}

function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [sourceUrl, setSourceUrl] = useState("");
  const [executionText, setExecutionText] = useState("");
  const [outcome, setOutcome] = useState<OperationOutcome | null>(null);
  const activeOperation = useRef<string | null>(null);

  const refresh = useCallback(async function refresh(): Promise<void> {
    setRefreshing(true);
    try {
      const next = await invokeParsed("sync_manifest_state", appStateSchema);
      startTransition(() => {
        setState(next);
      });
      setError(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    invokeParsed("load_cached_manifest_state", cachedStateSchema)
      .then((cached) => {
        if (cached !== null) {
          setState(cached);
        }
        return refresh();
      })
      .catch((reason: unknown) => {
        setError(errorText(reason));
      });
  }, [refresh]);

  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let disposed = false;
    const subscriptions = [
      listen<unknown>(SCHEDULED_SYNC_EVENT, (event) => {
        try {
          const scheduled = scheduledSyncSchema.parse(event.payload);
          if (scheduled.kind === "updated") {
            startTransition(() => {
              setState(scheduled.state);
            });
          } else {
            setError(scheduled.message);
          }
        } catch (reason) {
          setError(errorText(reason));
        }
      }),
      listen<unknown>(OPERATION_OUTPUT_EVENT, (event) => {
        try {
          const output = operationOutputSchema.parse(event.payload);
          if (output.operationId === activeOperation.current) {
            setExecutionText((current) => `${current}${output.stream === "stderr" ? "[stderr] " : ""}${output.text}`.slice(-MAX_VISIBLE_LOG_CHARACTERS));
          }
        } catch (reason) {
          setError(errorText(reason));
        }
      })
    ];
    Promise.all(subscriptions)
      .then((listeners) => {
        if (disposed) {
          for (const unlisten of listeners) {
            unlisten();
          }
        } else {
          unlisteners.push(...listeners);
        }
      })
      .catch((reason: unknown) => {
        setError(errorText(reason));
      });
    return (): void => {
      disposed = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, []);

  const runOperation = useCallback(async function runOperation(id: string, command: string, args: Record<string, unknown>): Promise<OperationOutcome> {
    const currentOperation = operationId(id);
    activeOperation.current = currentOperation;
    setExecutionText("");
    setOutcome(null);
    setBusyId(id);
    try {
      const result = await invokeParsed(command, operationOutcomeSchema, { ...args, operationId: currentOperation });
      setOutcome(result);
      return result;
    } finally {
      activeOperation.current = null;
      setBusyId(null);
    }
  }, []);

  const changeItem = useCallback(
    async function changeItem(item: CatalogItem): Promise<void> {
      setError(null);
      const args = { sourceId: item.sourceId, localId: item.localId };
      if (item.status === "installed" || item.status === "removed") {
        await runOperation(item.id, "uninstall_item", args);
      } else if (item.status === "conflict") {
        try {
          await runOperation(item.id, "install_item", args);
        } catch (reason) {
          if (!errorText(reason).includes("already exists and is not an unmodified owned destination")) {
            throw reason;
          }
          const replace = await confirm(`${item.id} differs from the published item. Replace every conflicting destination? Existing content is moved to a retained backup first.`, {
            title: "Replace unmanaged destinations",
            kind: "warning",
            okLabel: "Back Up and Replace",
            cancelLabel: "Cancel"
          });
          if (!replace) {
            return;
          }
          await runOperation(item.id, "replace_item", args);
        }
      } else {
        await runOperation(item.id, "install_item", args);
      }
      await refresh();
    },
    [refresh, runOperation]
  );

  const runItemAction = useCallback(
    async function runItemAction(item: CatalogItem, actionId: string): Promise<void> {
      await runOperation(item.id, "run_item_action", { sourceId: item.sourceId, localId: item.localId, actionId });
      await refresh();
    },
    [refresh, runOperation]
  );

  const runSourceAction = useCallback(
    async function runSourceAction(source: SourceState, actionId: string): Promise<void> {
      await runOperation(source.sourceId, "run_source_action", { sourceId: source.sourceId, actionId });
      await refresh();
    },
    [refresh, runOperation]
  );

  const runBulk = useCallback(
    async function runBulk(source: SourceState, uninstall: boolean): Promise<void> {
      setBusyId("bulk");
      try {
        const plan = await invokeParsed("plan_bulk_items", bulkPlanSchema, { sourceId: source.sourceId, uninstall });
        const selected = plan.entries.filter((entry) => entry.willRun);
        if (selected.length === 0) {
          await message(uninstall ? "No installed items can be removed." : "No available items or updates can be installed.", { title: "Nothing to do", kind: "info" });
          return;
        }
        const approved = await confirm(`${uninstall ? "Uninstall" : "Install or update"} these items?\n\n${selected.map((entry) => entry.id).join("\n")}`, {
          title: uninstall ? "Uninstall source items" : "Install source items",
          kind: uninstall ? "warning" : "info",
          okLabel: uninstall ? "Uninstall" : "Install",
          cancelLabel: "Cancel"
        });
        if (!approved) {
          return;
        }
        const currentOperation = operationId("bulk");
        activeOperation.current = currentOperation;
        setExecutionText("");
        const result = await invokeParsed("run_bulk_items", bulkResultSchema, { sourceId: source.sourceId, uninstall, operationId: currentOperation });
        if (result.failures.length > 0) {
          setError(`${String(result.completed.length)} completed; ${result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; ")}`);
        }
        await refresh();
      } finally {
        activeOperation.current = null;
        setBusyId(null);
      }
    },
    [refresh]
  );

  async function addSource(event: SyntheticEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const url = sourceUrl.trim();
    if (url.length === 0 || busyId !== null) {
      return;
    }
    setBusyId("source-add");
    let prepared: PreparedSource | null = null;
    try {
      prepared = await invokeParsed("prepare_source", preparedSourceSchema, { url });
      const prompt = preparedSourcePrompt(prepared);
      const approved = await confirm(prompt.text, { title: prompt.title, kind: prompt.kind, okLabel: prompt.okLabel, cancelLabel: "Cancel" });
      if (!approved) {
        await invokeParsed("cancel_prepared_source", unitSchema, { token: prepared.token });
        return;
      }
      const next = await invokeParsed("confirm_source", appStateSchema, { token: prepared.token, acceptExecutableTrust: prepared.executable });
      setState(next);
      setSourceUrl("");
      setError(null);
    } catch (reason) {
      setError(errorText(reason));
      if (prepared !== null) {
        invokeParsed("cancel_prepared_source", unitSchema, { token: prepared.token }).catch(() => undefined);
      }
    } finally {
      setBusyId(null);
    }
  }

  async function addDefaultSource(): Promise<void> {
    setBusyId("source-add");
    try {
      const next = await invokeParsed("add_default_manifest_source", appStateSchema);
      setState(next);
      setError(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusyId(null);
    }
  }

  async function changeTrust(source: SourceState, trusted: boolean): Promise<void> {
    const approved = await confirm(
      trusted
        ? `Trust ${source.name}? Its present and future code may run unsandboxed with your full filesystem, process, and network access, including scheduled background update hooks.`
        : `Revoke executable trust for ${source.name}? Hooks and actions will be blocked immediately.`,
      { title: trusted ? "Grant executable trust" : "Revoke executable trust", kind: "warning", okLabel: trusted ? "Grant Trust" : "Revoke", cancelLabel: "Cancel" }
    );
    if (!approved) {
      return;
    }
    setBusyId(source.sourceId);
    try {
      const next = await invokeParsed("set_source_trust", appStateSchema, { sourceId: source.sourceId, trusted });
      setState(next);
      if (trusted && source.trustRequired) {
        await refresh();
      }
      setError(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusyId(null);
    }
  }

  async function removeSource(source: SourceState): Promise<void> {
    setBusyId(source.sourceId);
    try {
      const plan = await invokeParsed("plan_source_removal", sourceRemovalPlanSchema, { sourceId: source.sourceId });
      const modified = plan.items.flatMap((item) => item.paths.filter((path) => path.modified).map((path) => `${item.id}: ${path.path}`));
      const executionWarning = plan.executableCleanup && !source.trusted ? "\n\nThis also grants one-time approval for the source's uninstall hooks." : "";
      const modifiedWarning = modified.length === 0 ? "" : `\n\nLocally modified managed paths will be permanently deleted without backup:\n${modified.join("\n")}`;
      const approved = await confirm(`Remove ${source.name} only after all ${String(plan.items.length)} installed items are completely uninstalled?${modifiedWarning}${executionWarning}`, {
        title: "Clean up and remove source",
        kind: "warning",
        okLabel: "Uninstall and Remove",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      const currentOperation = operationId(source.sourceId);
      activeOperation.current = currentOperation;
      setExecutionText("");
      const result = await invokeParsed("remove_manifest_source", bulkResultSchema, {
        sourceId: source.sourceId,
        acknowledgeModifiedPaths: modified.length > 0,
        approveCleanupExecution: plan.executableCleanup && !source.trusted,
        operationId: currentOperation
      });
      if (result.failures.length > 0) {
        setError(`The source was retained because cleanup failed: ${result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; ")}`);
      }
      await refresh();
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      activeOperation.current = null;
      setBusyId(null);
    }
  }

  const itemsBySource = useMemo(() => {
    const grouped = new Map<string, readonly CatalogItem[]>();
    if (state === null) {
      return grouped;
    }
    for (const source of state.sources) {
      grouped.set(
        source.sourceId,
        state.items.filter((item) => item.sourceId === source.sourceId)
      );
    }
    return grouped;
  }, [state]);

  return (
    <main className="app-shell">
      <div className="notice-stack">
        <Notice
          error={error}
          state={state}
          onDismiss={() => {
            setError(null);
          }}
        />
        <ExecutionPanel text={executionText} outcome={outcome} onRevealError={setError} />
      </div>
      <section className="catalog-stage" aria-labelledby="catalog-heading">
        <div className="catalog">
          <div className="section-heading">
            <div>
              <Heading id="catalog-heading" as="h2" size="5">
                Catalog
              </Heading>
              <Text as="p" color="gray" size="2">
                {state === null ? "Loading manifest sources…" : `${String(state.items.length)} items from ${String(state.sources.length)} sources`}
              </Text>
            </div>
            <div className="catalog-actions">
              <SourcesDialog
                open={sourcesOpen}
                state={state}
                url={sourceUrl}
                busyId={busyId}
                onOpenChange={setSourcesOpen}
                onUrlChange={setSourceUrl}
                onAdd={addSource}
                onAddDefault={addDefaultSource}
                onTrust={changeTrust}
                onRemove={removeSource}
              />
              <Button
                className="refresh-button"
                disabled={refreshing || busyId !== null}
                loading={refreshing}
                variant="surface"
                onClick={() => {
                  refresh().catch((reason: unknown) => {
                    setError(errorText(reason));
                  });
                }}
              >
                Refresh
              </Button>
              <AboutDialog />
            </div>
          </div>
          {state === null ? (
            <Card className="loading-card">
              <Spinner /> <Text color="gray">Reading validated source snapshots…</Text>
            </Card>
          ) : (
            <div className="skill-list">
              {state.sources.map((source) => (
                <SourceGroup
                  key={source.sourceKey}
                  source={source}
                  items={itemsBySource.get(source.sourceId) ?? []}
                  busyId={busyId}
                  onChange={changeItem}
                  onItemAction={runItemAction}
                  onSourceAction={runSourceAction}
                  onBulk={runBulk}
                  onError={setError}
                />
              ))}
            </div>
          )}
        </div>
      </section>
      <footer>
        <Text color="gray" size="1">
          Namespaces are repository-published; executable trust is repository-bound.
        </Text>
      </footer>
    </main>
  );
}

export default App;
