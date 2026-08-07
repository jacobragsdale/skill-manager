import { startTransition, useCallback, useEffect, useMemo, useState } from "react";
import type { JSX, SyntheticEvent } from "react";
import { Badge, Button, Callout, Card, Code, Dialog, Heading, Spinner, Text, TextField } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { z } from "zod";
import "./App.css";

const SCHEDULED_SYNC_EVENT = "scheduled-sync";

const itemStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict", "sourceConflict"]);
const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
const destinationAnchorSchema = z.enum(["home", "config", "data", "localData", "cache"]);
const catalogErrorSchema = z.strictObject({ path: z.string().min(1), message: z.string().min(1) }).readonly();
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
    source: z.string().min(1),
    destination: destinationSchema,
    status: itemStatusSchema
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
    catalogErrors: z.array(catalogErrorSchema).readonly()
  })
  .readonly();
const itemReferenceSchema = z.strictObject({ id: z.string().min(1), sourceId: z.string().min(2), localId: z.string().min(1) }).readonly();
const itemFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const autoUpdateReportSchema = z.strictObject({ updatedItems: z.array(itemReferenceSchema).readonly(), failedItems: z.array(itemFailureSchema).readonly() }).readonly();
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
    itemCount: z.number().int().nonnegative()
  })
  .readonly();
const operationOutcomeSchema = z.strictObject({ backupPaths: z.array(z.string().min(1)).readonly() }).readonly();
const bulkPlanEntrySchema = z.strictObject({ id: z.string().min(1), localId: z.string().min(1), status: itemStatusSchema, willRun: z.boolean() }).readonly();
const bulkPlanSchema = z.strictObject({ sourceId: z.string().min(2), uninstall: z.boolean(), entries: z.array(bulkPlanEntrySchema).readonly() }).readonly();
const bulkFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const bulkResultSchema = z.strictObject({ completed: z.array(z.string().min(1)).readonly(), failures: z.array(bulkFailureSchema).readonly() }).readonly();
const removalPathSchema = z.strictObject({ path: z.string().min(1), modified: z.boolean() }).readonly();
const removalItemSchema = z.strictObject({ id: z.string().min(1), paths: z.array(removalPathSchema).readonly() }).readonly();
const sourceRemovalPlanSchema = z.strictObject({ sourceId: z.string().min(2), items: z.array(removalItemSchema).readonly() }).readonly();
const scheduledSyncSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("updated"), state: appStateSchema }).readonly(),
  z.strictObject({ kind: z.literal("failed"), message: z.string().min(1) }).readonly()
]);
const cachedStateSchema = appStateSchema.nullable();
const unitSchema = z.null();

type AppState = z.infer<typeof appStateSchema>;
type CatalogItem = z.infer<typeof itemSchema>;
type ItemStatus = z.infer<typeof itemStatusSchema>;
type SourceState = z.infer<typeof sourceSchema>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

async function invokeParsed<T>(command: string, schema: z.ZodType<T>, args?: Record<string, unknown>): Promise<T> {
  const payload = args === undefined ? await invoke<unknown>(command) : await invoke<unknown>(command, args);
  return schema.parse(payload);
}

function errorText(reason: unknown): string {
  return reason instanceof z.ZodError ? `Skill Manager returned invalid data: ${z.prettifyError(reason)}` : String(reason);
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
  }
}

function statusColor(status: ItemStatus): AccentColor {
  switch (status) {
    case "installed":
      return "green";
    case "updateAvailable":
      return "blue";
    case "available":
      return "gray";
    case "removed":
    case "conflict":
    case "sourceConflict":
      return "amber";
    case "modified":
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
    case "modified":
      return "Protected";
    case "sourceConflict":
      return "Owned Elsewhere";
  }
}

function reportMessage(state: AppState): string | null {
  const parts: string[] = [];
  if (state.autoUpdateReport.updatedItems.length > 0) {
    parts.push(`Updated ${state.autoUpdateReport.updatedItems.map((item) => item.id).join(", ")}.`);
  }
  if (state.autoUpdateReport.failedItems.length > 0) {
    parts.push(`Background updates failed: ${state.autoUpdateReport.failedItems.map((item) => `${item.id}: ${item.message}`).join("; ")}.`);
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

function ItemCard({ item, busy, onChange, onError }: Readonly<{ item: CatalogItem; busy: boolean; onChange: (item: CatalogItem) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const protectedItem = item.status === "modified" || item.status === "sourceConflict";
  return (
    <Card className="skill-card item-card">
      <div className="skill-copy">
        <div className="skill-title-row">
          <Heading as="h4" size="3">
            {item.name}
          </Heading>
          <Badge color={statusColor(item.status)}>{statusLabel(item.status)}</Badge>
        </div>
        <Text as="p" color="gray" size="2">
          {item.description}
        </Text>
        <Code className="canonical-id" color="gray" size="1" variant="ghost">
          {item.id}
        </Code>
        <details className="item-details">
          <summary>Source and destination</summary>
          <dl>
            <dt>Source</dt>
            <dd>{item.source}</dd>
            <dt>Destination</dt>
            <dd>
              {item.destination.anchor}: {item.destination.path}
            </dd>
          </dl>
          <Button
            size="1"
            variant="ghost"
            onClick={() => {
              revealItemInDir(item.destination.path).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Reveal destination
          </Button>
        </details>
      </div>
      <div className="item-actions">
        <Button
          className="skill-action skill-action-primary"
          disabled={busy || protectedItem}
          loading={busy}
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
  busyIds,
  allBusy,
  onItemChange,
  onBulk,
  onError
}: Readonly<{
  source: SourceState;
  items: readonly CatalogItem[];
  busyIds: ReadonlySet<string>;
  allBusy: boolean;
  onItemChange: (item: CatalogItem) => Promise<void>;
  onBulk: (source: SourceState, uninstall: boolean) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  return (
    <section className="source-group">
      <div className="source-heading">
        <div>
          <div className="source-title-row">
            <Heading as="h3" size="4">
              {source.name}
            </Heading>
            <Badge color={source.status === "fresh" ? "green" : source.status === "cached" ? "gray" : "red"}>{source.status}</Badge>
          </div>
          <Text as="p" color="gray" size="2">
            {source.description}
          </Text>
          <Button
            className="source-link"
            size="1"
            variant="ghost"
            onClick={() => {
              openUrl(source.url).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            {source.sourceId}
          </Button>
        </div>
        <div className="source-group-actions">
          <Button
            size="1"
            variant="soft"
            disabled={allBusy}
            onClick={() => {
              onBulk(source, false).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Install All
          </Button>
          <Button
            size="1"
            variant="soft"
            color="red"
            disabled={allBusy}
            onClick={() => {
              onBulk(source, true).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Uninstall All
          </Button>
        </div>
      </div>
      {source.message === null ? null : (
        <Callout.Root className="app-callout" color="red">
          <Callout.Text>{source.message}</Callout.Text>
        </Callout.Root>
      )}
      {source.catalogErrors.length === 0 ? null : (
        <Callout.Root className="app-callout" color="amber">
          <Callout.Text>{source.catalogErrors.map((catalogError) => `${catalogError.path}: ${catalogError.message}`).join(" · ")}</Callout.Text>
        </Callout.Root>
      )}
      <div className="skills-list">
        {items.map((item) => (
          <ItemCard key={item.id} item={item} busy={busyIds.has(item.id)} onChange={onItemChange} onError={onError} />
        ))}
        {items.length === 0 ? <Text color="gray">This source currently publishes no valid installs.</Text> : null}
      </div>
    </section>
  );
}

function ManageSourcesDialog({
  open,
  state,
  adding,
  removing,
  onOpenChange,
  onAdd,
  onAddDefault,
  onRemove,
  onError
}: Readonly<{
  open: boolean;
  state: AppState | null;
  adding: boolean;
  removing: ReadonlySet<string>;
  onOpenChange: (open: boolean) => void;
  onAdd: (url: string) => Promise<void>;
  onAddDefault: () => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const [url, setUrl] = useState("");

  function submit(event: SyntheticEvent<HTMLFormElement>): void {
    event.preventDefault();
    onAdd(url)
      .then(() => {
        setUrl("");
      })
      .catch((reason: unknown) => {
        onError(errorText(reason));
      });
  }

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="640px">
        <Dialog.Title>Manage sources</Dialog.Title>
        <Dialog.Description>Add a Git repository that publishes a top-level skill-manager.json manifest.</Dialog.Description>
        <form className="source-form" onSubmit={submit}>
          <TextField.Root
            value={url}
            placeholder="https://github.com/owner/repository"
            onChange={(event) => {
              setUrl(event.currentTarget.value);
            }}
          />
          <Button type="submit" disabled={adding || url.trim().length === 0} loading={adding}>
            Add Source
          </Button>
          {state?.sources.some((source) => source.builtIn) === false ? (
            <Button
              type="button"
              variant="soft"
              disabled={adding}
              onClick={() => {
                onAddDefault().catch((reason: unknown) => {
                  onError(errorText(reason));
                });
              }}
            >
              Add default Skillbook
            </Button>
          ) : null}
        </form>
        <div className="managed-sources">
          {state?.sources.map((source) => (
            <Card key={source.sourceKey} className="managed-source">
              <div>
                <Heading as="h3" size="2">
                  {source.name}
                </Heading>
                <Text as="p" color="gray" size="1">
                  {source.url}
                </Text>
              </div>
              <Button
                color="red"
                size="1"
                variant="soft"
                loading={removing.has(source.sourceId)}
                disabled={removing.has(source.sourceId)}
                onClick={() => {
                  onRemove(source).catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                Remove…
              </Button>
            </Card>
          ))}
        </div>
        <div className="dialog-actions">
          <Dialog.Close>
            <Button variant="soft">Done</Button>
          </Dialog.Close>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}

export default function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [adding, setAdding] = useState(false);
  const [sourceDialogOpen, setSourceDialogOpen] = useState(false);
  const [busyItems, setBusyItems] = useState<ReadonlySet<string>>(new Set());
  const [busySources, setBusySources] = useState<ReadonlySet<string>>(new Set());

  const applyState = useCallback((next: AppState): void => {
    startTransition(() => {
      setState(next);
    });
  }, []);

  const loadCached = useCallback(async (): Promise<void> => {
    const cached = await invokeParsed("load_cached_manifest_state", cachedStateSchema);
    if (cached !== null) {
      applyState(cached);
    }
  }, [applyState]);

  const synchronize = useCallback(async (): Promise<void> => {
    setSyncing(true);
    try {
      applyState(await invokeParsed("sync_manifest_state", appStateSchema));
      setError(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setSyncing(false);
    }
  }, [applyState]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<unknown>(SCHEDULED_SYNC_EVENT, (event) => {
      if (disposed) {
        return;
      }
      const scheduled = scheduledSyncSchema.parse(event.payload);
      if (scheduled.kind === "updated") {
        applyState(scheduled.state);
        setError(null);
      } else {
        setError(scheduled.message);
      }
    })
      .then((stop) => {
        if (disposed) {
          stop();
        } else {
          unlisten = stop;
        }
      })
      .catch((reason: unknown) => {
        if (!disposed) {
          setError(errorText(reason));
        }
      });
    loadCached()
      .then(synchronize)
      .catch((reason: unknown) => {
        if (!disposed) {
          setError(errorText(reason));
        }
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyState, loadCached, synchronize]);

  const itemsBySource = useMemo(() => {
    const grouped = new Map<string, CatalogItem[]>();
    for (const item of state?.items ?? []) {
      const items = grouped.get(item.sourceKey) ?? [];
      items.push(item);
      grouped.set(item.sourceKey, items);
    }
    return grouped;
  }, [state]);

  async function refreshCached(): Promise<void> {
    await loadCached();
    setError(null);
  }

  async function changeItem(item: CatalogItem): Promise<void> {
    if (item.status === "modified" || item.status === "sourceConflict") {
      return;
    }
    let command: "install_item" | "replace_item" | "uninstall_item";
    if (item.status === "conflict") {
      const approved = await confirm(`Back up the existing destination and let Skill Manager manage ${item.name}?`, {
        title: "Manage existing destination",
        kind: "warning",
        okLabel: "Back Up and Replace",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      command = "replace_item";
    } else if (item.status === "installed" || item.status === "removed") {
      command = "uninstall_item";
    } else {
      command = "install_item";
    }
    setBusyItems((current) => new Set(current).add(item.id));
    try {
      const outcome = await invokeParsed(command, operationOutcomeSchema, { sourceId: item.sourceId, localId: item.localId });
      if (outcome.backupPaths.length > 0) {
        await message(`The previous destination was backed up at ${outcome.backupPaths.join(", ")}.`, { title: "Backup created", kind: "info" });
      }
      await refreshCached();
    } finally {
      setBusyItems((current) => {
        const next = new Set(current);
        next.delete(item.id);
        return next;
      });
    }
  }

  async function runBulk(source: SourceState, uninstall: boolean): Promise<void> {
    setBusySources((current) => new Set(current).add(source.sourceId));
    try {
      const plan = await invokeParsed("plan_bulk_items", bulkPlanSchema, { sourceId: source.sourceId, uninstall });
      const count = plan.entries.filter((entry) => entry.willRun).length;
      if (count === 0) {
        await message(uninstall ? "No installed items can be removed." : "No available items or updates can be installed.", { title: "Nothing to do", kind: "info" });
        return;
      }
      const approved = await confirm(`${uninstall ? "Uninstall" : "Install or update"} ${String(count)} item${count === 1 ? "" : "s"} from ${source.name}?`, {
        title: uninstall ? "Uninstall all" : "Install all",
        kind: uninstall ? "warning" : "info",
        okLabel: uninstall ? "Uninstall" : "Install",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      const result = await invokeParsed("run_bulk_items", bulkResultSchema, { sourceId: source.sourceId, uninstall });
      if (result.failures.length > 0) {
        setError(result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; "));
      }
      await loadCached();
    } finally {
      setBusySources((current) => {
        const next = new Set(current);
        next.delete(source.sourceId);
        return next;
      });
    }
  }

  async function addSource(url: string): Promise<void> {
    setAdding(true);
    try {
      const prepared = await invokeParsed("prepare_source", preparedSourceSchema, { url: url.trim() });
      const approved = await confirm(
        `${prepared.name} (${prepared.sourceId}) publishes ${String(prepared.itemCount)} valid install${prepared.itemCount === 1 ? "" : "s"} at ${prepared.commit.slice(0, 12)}. Add this source?`,
        { title: "Confirm source", kind: "info", okLabel: "Add Source", cancelLabel: "Cancel" }
      );
      if (!approved) {
        await invokeParsed("cancel_prepared_source", unitSchema, { token: prepared.token });
        return;
      }
      applyState(await invokeParsed("confirm_source", appStateSchema, { token: prepared.token }));
      setError(null);
    } finally {
      setAdding(false);
    }
  }

  async function addDefault(): Promise<void> {
    setAdding(true);
    try {
      applyState(await invokeParsed("add_default_manifest_source", appStateSchema));
      setError(null);
    } finally {
      setAdding(false);
    }
  }

  async function removeSource(source: SourceState): Promise<void> {
    setBusySources((current) => new Set(current).add(source.sourceId));
    try {
      const plan = await invokeParsed("plan_source_removal", sourceRemovalPlanSchema, { sourceId: source.sourceId });
      const modified = plan.items.flatMap((item) => item.paths).filter((path) => path.modified);
      const warning = modified.length === 0 ? "" : `\n\nThis will also delete locally modified paths:\n${modified.map((path) => path.path).join("\n")}`;
      const approved = await confirm(`Uninstall ${String(plan.items.length)} managed item${plan.items.length === 1 ? "" : "s"} and remove ${source.name}?${warning}`, {
        title: "Remove source",
        kind: "warning",
        okLabel: "Remove",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      const result = await invokeParsed("remove_manifest_source", bulkResultSchema, { sourceId: source.sourceId, acknowledgeModifiedPaths: modified.length > 0 });
      if (result.failures.length > 0) {
        setError(result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; "));
      }
      await loadCached();
    } finally {
      setBusySources((current) => {
        const next = new Set(current);
        next.delete(source.sourceId);
        return next;
      });
    }
  }

  const checked = state === null ? "Not checked yet" : new Date(state.checkedAtEpochSeconds * 1000).toLocaleString();
  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <Heading as="h1" size="7">
            Skill Manager
          </Heading>
          <Text as="p" color="gray">
            Install files and directories from Git sources you choose.
          </Text>
        </div>
        <div className="catalog-actions">
          <Button
            variant="soft"
            onClick={() => {
              setSourceDialogOpen(true);
            }}
          >
            Manage Sources
          </Button>
          <Button loading={syncing} disabled={syncing} onClick={() => void synchronize()}>
            Check Now
          </Button>
        </div>
      </header>
      <div className="sync-meta">
        <Text color="gray" size="1">
          Last checked: {checked}
        </Text>
      </div>
      <Notice
        error={error}
        state={state}
        onDismiss={() => {
          setError(null);
        }}
      />
      {state === null ? (
        <div className="loading-state">
          <Spinner size="3" />
          <Text color="gray">Loading sources…</Text>
        </div>
      ) : (
        <div className="sources-list">
          {state.sources.map((source) => (
            <SourceGroup
              key={source.sourceKey}
              source={source}
              items={itemsBySource.get(source.sourceKey) ?? []}
              busyIds={busyItems}
              allBusy={busySources.has(source.sourceId)}
              onItemChange={changeItem}
              onBulk={runBulk}
              onError={setError}
            />
          ))}
        </div>
      )}
      <ManageSourcesDialog
        open={sourceDialogOpen}
        state={state}
        adding={adding}
        removing={busySources}
        onOpenChange={setSourceDialogOpen}
        onAdd={addSource}
        onAddDefault={addDefault}
        onRemove={removeSource}
        onError={setError}
      />
    </main>
  );
}
