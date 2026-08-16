import { startTransition, useCallback, useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import { Button, Heading, Spinner, Text } from "@radix-ui/themes";
import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { AgentProfilesDialog } from "./components/AgentProfilesDialog";
import { AgentSetupNotice } from "./components/AgentSetupNotice";
import { ManageSourcesDialog } from "./components/ManageSourcesDialog";
import { Notice } from "./components/Notice";
import { SourceGroup } from "./components/SourceGroup";
import { errorText, invokeParsed, SCHEDULED_SYNC_EVENT } from "./ipc/client";
import { appStateSchema, bulkPlanSchema, bulkResultSchema, cachedStateSchema, operationOutcomeSchema, preparedSourceSchema, scheduledSyncSchema, sourceRemovalPlanSchema } from "./ipc/schemas";
import type { AppState, BulkAction, CatalogItem, ListedSource, RepositoryState, SourceState } from "./ipc/schemas";
import { commandForStatus, hasDetectedAgent, itemCommandArgs, reviewBulk, reviewReplace, reviewReset } from "./lib/status";
import "./App.css";

export default function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [adding, setAdding] = useState(false);
  const [sourceDialogOpen, setSourceDialogOpen] = useState(false);
  const [agentDialogOpen, setAgentDialogOpen] = useState(false);
  const [agentSetupPrompted, setAgentSetupPrompted] = useState(false);
  const [busyItems, setBusyItems] = useState<ReadonlySet<string>>(new Set());
  const [busySources, setBusySources] = useState<ReadonlySet<string>>(new Set());
  const [resetting, setResetting] = useState(false);

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
      try {
        const scheduled = scheduledSyncSchema.parse(event.payload);
        if (scheduled.kind === "updated") {
          applyState(scheduled.state);
          setError(null);
        } else {
          setError(scheduled.message);
        }
      } catch (reason: unknown) {
        setError(errorText(reason));
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
      .catch((reason: unknown) => {
        if (!disposed) {
          setError(errorText(reason));
        }
      })
      .then(() => {
        if (!disposed) {
          return synchronize();
        }
        return undefined;
      })
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

  useEffect(() => {
    if (state === null) {
      return;
    }
    if (hasDetectedAgent(state.agentProfiles)) {
      setAgentSetupPrompted(false);
      return;
    }
    if (!agentSetupPrompted) {
      setAgentDialogOpen(true);
      setAgentSetupPrompted(true);
    }
  }, [agentSetupPrompted, state]);

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

  async function changeItem(item: CatalogItem, componentId?: string): Promise<void> {
    const component = componentId === undefined ? undefined : item.components.find((entry) => entry.id === componentId);
    if (componentId !== undefined && component === undefined) {
      return;
    }
    const command = commandForStatus(component?.status ?? item.status);
    if (command === null) {
      return;
    }
    if (command === "replace_item" && !(await reviewReplace())) {
      return;
    }
    const trustApproved = command !== "uninstall_item";
    setBusyItems((current) => new Set(current).add(item.id));
    try {
      const outcome = await invokeParsed(command, operationOutcomeSchema, itemCommandArgs(item, componentId, { trustApproved }));
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

  async function runBulk(source: SourceState, action: BulkAction): Promise<void> {
    setBusySources((current) => new Set(current).add(source.sourceId));
    try {
      const plan = await invokeParsed("plan_bulk_items", bulkPlanSchema, { sourceId: source.sourceId, action });
      const count = plan.entries.filter((entry) => entry.willRun).length;
      if (count === 0) {
        await message("No items are currently eligible for that action.", { title: "Nothing to do", kind: "info" });
        return;
      }
      if (action === "replace" && !(await reviewBulk(source, action, plan))) {
        return;
      }
      const result = await invokeParsed("run_bulk_items", bulkResultSchema, { sourceId: source.sourceId, action, trustApproved: action !== "uninstall" });
      if (result.failures.length > 0) {
        setError(result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; "));
      }
      if (result.backupPaths.length > 0) {
        await message(`Previous destinations were backed up at ${result.backupPaths.join(", ")}.`, { title: "Backups created", kind: "info" });
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

  async function resetApp(): Promise<void> {
    if (!(await reviewReset())) {
      return;
    }
    setResetting(true);
    try {
      const result = await invokeParsed("reset_app", bulkResultSchema);
      if (result.failures.length > 0) {
        setError(result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; "));
      } else {
        const count = result.completed.length;
        const backups = result.backupPaths.length === 0 ? "" : `\n\nBacked up leftover files at ${result.backupPaths.join(", ")}.`;
        await message(
          count === 0
            ? `No leftover installs remained. All Agent Plugins data was cleared.${backups}`
            : `Removed ${String(count)} leftover install${count === 1 ? "" : "s"} and cleared all Agent Plugins data.${backups}`,
          { title: "App reset", kind: "info" }
        );
      }
      await synchronize();
    } finally {
      setResetting(false);
    }
  }

  async function addListedSource(repository: RepositoryState, listed: ListedSource): Promise<void> {
    setAdding(true);
    try {
      const prepared = await invokeParsed("prepare_source", preparedSourceSchema, { url: listed.url, repositoryKey: repository.repositoryKey });
      applyState(await invokeParsed("confirm_source", appStateSchema, { token: prepared.token }));
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
            Agent Plugins
          </Heading>
        </div>
        <div className="catalog-actions">
          <Button
            variant="soft"
            disabled={resetting}
            onClick={() => {
              setAgentDialogOpen(true);
            }}
          >
            Detected Agents
          </Button>
          <Button
            variant="soft"
            disabled={resetting}
            onClick={() => {
              setSourceDialogOpen(true);
            }}
          >
            Manage Sources
          </Button>
          <Button loading={syncing} disabled={syncing || resetting} onClick={() => void synchronize()}>
            Refresh
          </Button>
          <Button
            color="red"
            variant="soft"
            loading={resetting}
            disabled={resetting || syncing}
            onClick={() => {
              resetApp().catch((reason: unknown) => {
                setError(errorText(reason));
              });
            }}
          >
            Reset
          </Button>
        </div>
      </header>
      <div className="sync-meta">
        <Text color="gray" size="1">
          Last checked: {checked}
        </Text>
      </div>
      <AgentSetupNotice
        visible={state !== null && !hasDetectedAgent(state.agentProfiles)}
        onChoose={() => {
          setAgentDialogOpen(true);
        }}
      />
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
              allBusy={resetting || busySources.has(source.sourceId)}
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
        onAddListed={addListedSource}
        onRemove={removeSource}
        onError={setError}
      />
      <AgentProfilesDialog open={agentDialogOpen} profiles={state?.agentProfiles ?? []} onOpenChange={setAgentDialogOpen} />
    </main>
  );
}
