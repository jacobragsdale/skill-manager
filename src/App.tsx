import { startTransition, useCallback, useEffect, useMemo, useState } from "react";
import type { JSX, SyntheticEvent } from "react";
import { Badge, Button, Callout, Card, Dialog, Heading, Spinner, Text, TextField } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { z } from "zod";
import "./App.css";

const SCHEDULED_SYNC_EVENT = "scheduled-sync";

const itemStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict", "sourceConflict", "partiallyInstalled"]);
const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
const locatorKindSchema = z.enum(["git", "artifact"]);
const catalogErrorSchema = z.strictObject({ path: z.string().min(1), message: z.string().min(1) }).readonly();
const targetIdSchema = z.enum(["cursor", "claude-code", "codex", "opencode", "grok-build", "github-copilot"]);
const agentProfileSchema = z
  .strictObject({
    targetId: targetIdSchema,
    displayName: z.string().min(1),
    enabled: z.boolean(),
    scopes: z.array(z.string().min(1)).readonly(),
    dialectId: z.string().min(1),
    detected: z.boolean(),
    detectedVersion: z.string().min(1).nullable(),
    detectionMessage: z.string().min(1).nullable(),
    verificationGuidance: z.string().min(1),
    reloadGuidance: z.string().min(1)
  })
  .readonly();
const componentSchema = z.strictObject({ id: z.string().min(1), kind: z.string().min(1) }).readonly();
const capabilitySchema = z.discriminatedUnion("level", [
  z.strictObject({ level: z.literal("native") }).readonly(),
  z.strictObject({ level: z.literal("losslessTranslation") }).readonly(),
  z.strictObject({ level: z.literal("lossyTranslation"), losses: z.array(z.string().min(1)).readonly() }).readonly(),
  z.strictObject({ level: z.literal("unsupported"), reason: z.string().min(1) }).readonly(),
  z.strictObject({ level: z.literal("blocked"), reason: z.string().min(1), requiredAction: z.string().min(1) }).readonly()
]);
const compatibilitySchema = z.strictObject({ componentId: z.string().min(1), targetId: z.string().min(1), capability: capabilitySchema }).readonly();
const itemSchema = z
  .strictObject({
    id: z.string().min(3),
    localId: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    sourceName: z.string().min(1),
    sourceUrl: z.string().min(1),
    locatorKind: locatorKindSchema,
    name: z.string().min(1),
    description: z.string().min(1),
    manualInvocation: z.boolean(),
    source: z.string().min(1),
    sourceIsDirectory: z.boolean(),
    manifestVersion: z.number().int().min(1).max(2),
    components: z.array(componentSchema).readonly(),
    compatibility: z.array(compatibilitySchema).readonly(),
    destination: z.string().min(1).nullable(),
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
    locatorKind: locatorKindSchema,
    repositoryKey: z.string().min(1).nullable(),
    builtIn: z.boolean(),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    commit: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    catalogErrors: z.array(catalogErrorSchema).readonly()
  })
  .readonly();
const listedSourceSchema = z
  .strictObject({ name: z.string().min(1), description: z.string().min(1), locatorKind: locatorKindSchema, url: z.string().min(1), sourceId: z.string().min(2).nullable(), alreadyAdded: z.boolean() })
  .readonly();
const repositorySchema = z
  .strictObject({
    repositoryId: z.string().min(2),
    repositoryKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    locatorKind: locatorKindSchema,
    url: z.string().min(1),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    revision: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    sources: z.array(listedSourceSchema).readonly()
  })
  .readonly();
const itemReferenceSchema = z.strictObject({ id: z.string().min(1), sourceId: z.string().min(2), localId: z.string().min(1) }).readonly();
const itemFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const autoUpdateReportSchema = z.strictObject({ updatedItems: z.array(itemReferenceSchema).readonly(), failedItems: z.array(itemFailureSchema).readonly() }).readonly();
const appStateSchema = z
  .strictObject({
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    autoUpdateReport: autoUpdateReportSchema,
    repositories: z.array(repositorySchema).readonly(),
    sources: z.array(sourceSchema).readonly(),
    items: z.array(itemSchema).readonly(),
    agentProfiles: z.array(agentProfileSchema).readonly()
  })
  .readonly();
const preparedSourceSchema = z
  .strictObject({
    token: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    locatorKind: locatorKindSchema,
    commit: z.string().min(1),
    itemCount: z.number().int().nonnegative()
  })
  .readonly();
const preparedRepositorySchema = z
  .strictObject({
    token: z.string().min(1),
    repositoryId: z.string().min(2),
    repositoryKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    locatorKind: locatorKindSchema,
    revision: z.string().min(1),
    sourceCount: z.number().int().nonnegative()
  })
  .readonly();
const operationOutcomeSchema = z.strictObject({ backupPaths: z.array(z.string().min(1)).readonly() }).readonly();
const bulkActionSchema = z.enum(["install", "replace", "uninstall"]);
const bulkPlanEntrySchema = z.strictObject({ id: z.string().min(1), localId: z.string().min(1), status: itemStatusSchema, willRun: z.boolean() }).readonly();
const bulkPlanSchema = z.strictObject({ sourceId: z.string().min(2), action: bulkActionSchema, entries: z.array(bulkPlanEntrySchema).readonly() }).readonly();
const bulkFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
const bulkResultSchema = z
  .strictObject({ completed: z.array(z.string().min(1)).readonly(), failures: z.array(bulkFailureSchema).readonly(), backupPaths: z.array(z.string().min(1)).readonly() })
  .readonly();
const removalPathSchema = z.strictObject({ path: z.string().min(1), modified: z.boolean() }).readonly();
const removalItemSchema = z.strictObject({ id: z.string().min(1), paths: z.array(removalPathSchema).readonly() }).readonly();
const sourceRemovalPlanSchema = z.strictObject({ sourceId: z.string().min(2), items: z.array(removalItemSchema).readonly() }).readonly();
const resourcePreviewSchema = z
  .strictObject({ id: z.string().min(1), kind: z.string().min(1), identity: z.string().min(1), consumers: z.array(z.string().min(1)).readonly(), shared: z.boolean() })
  .readonly();
const installPreviewSchema = z
  .strictObject({
    installationId: z.string().min(1),
    compatibility: z.array(compatibilitySchema).readonly(),
    resources: z.array(resourcePreviewSchema).readonly(),
    warnings: z.array(z.string().min(1)).readonly(),
    trustTier: z.number().int().min(1).max(4),
    requiresApproval: z.boolean(),
    riskDetails: z.array(z.string().min(1)).readonly()
  })
  .readonly();
const targetCleanupPreviewSchema = z
  .strictObject({
    targetId: targetIdSchema,
    bindingCount: z.number().int().nonnegative(),
    resourcesRemoved: z.array(z.string().min(1)).readonly(),
    resourcesRetained: z.array(z.string().min(1)).readonly()
  })
  .readonly();
const agentEnablePreviewSchema = z.strictObject({ targetId: targetIdSchema, packages: z.array(installPreviewSchema).readonly() }).readonly();
const scheduledSyncSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("updated"), state: appStateSchema }).readonly(),
  z.strictObject({ kind: z.literal("failed"), message: z.string().min(1) }).readonly()
]);
const cachedStateSchema = appStateSchema.nullable();
const unitSchema = z.null();

type AppState = z.infer<typeof appStateSchema>;
type CatalogItem = z.infer<typeof itemSchema>;
type ItemStatus = z.infer<typeof itemStatusSchema>;
type LocatorKind = z.infer<typeof locatorKindSchema>;
type SourceState = z.infer<typeof sourceSchema>;
type RepositoryState = z.infer<typeof repositorySchema>;
type ListedSource = z.infer<typeof listedSourceSchema>;
type BulkAction = z.infer<typeof bulkActionSchema>;
type BulkPlan = z.infer<typeof bulkPlanSchema>;
type AgentProfile = z.infer<typeof agentProfileSchema>;
type TargetId = z.infer<typeof targetIdSchema>;
type InstallPreview = z.infer<typeof installPreviewSchema>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

async function invokeParsed<T>(command: string, schema: z.ZodType<T>, args?: Record<string, unknown>): Promise<T> {
  const payload = args === undefined ? await invoke<unknown>(command) : await invoke<unknown>(command, args);
  return schema.parse(payload);
}

function errorText(reason: unknown): string {
  return reason instanceof z.ZodError ? `Skill Manager returned invalid data: ${z.prettifyError(reason)}` : String(reason);
}

function repositoryBrowserUrl(repositoryUrl: string): string | null {
  try {
    const parsedUrl = new URL(repositoryUrl);
    if (parsedUrl.protocol !== "https:" && parsedUrl.protocol !== "ssh:") {
      return null;
    }
    const authority = parsedUrl.protocol === "https:" ? parsedUrl.host : parsedUrl.hostname;
    const browserUrl = new URL(`https://${authority}`);
    browserUrl.pathname = parsedUrl.pathname.endsWith(".git") ? parsedUrl.pathname.slice(0, -4) : parsedUrl.pathname;
    return browserUrl.href;
  } catch {
    return null;
  }
}

function repositoryPathBrowserUrl(repositoryUrl: string, commit: string, sourcePath: string, sourceIsDirectory: boolean): string | null {
  const browserUrl = repositoryBrowserUrl(repositoryUrl);
  if (browserUrl === null) {
    return null;
  }
  const parsedUrl = new URL(browserUrl);
  const repositoryPath = parsedUrl.pathname.replace(/\/$/u, "");
  if (parsedUrl.hostname === "github.com") {
    parsedUrl.pathname = `${repositoryPath}/${sourceIsDirectory ? "tree" : "blob"}/${commit}/${sourcePath}`;
  } else if (parsedUrl.hostname === "gitlab.com") {
    parsedUrl.pathname = `${repositoryPath}/-/${sourceIsDirectory ? "tree" : "blob"}/${commit}/${sourcePath}`;
  } else if (parsedUrl.hostname === "bitbucket.org") {
    parsedUrl.pathname = `${repositoryPath}/src/${commit}/${sourcePath}`;
  } else {
    return null;
  }
  return parsedUrl.href;
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
    case "partiallyInstalled":
      return "Partially Installed";
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
    case "partiallyInstalled":
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
    case "partiallyInstalled":
      return "Update";
    case "installed":
    case "removed":
      return "Uninstall";
    case "conflict":
      return "Replace…";
    case "modified":
      return "Protected";
    case "sourceConflict":
      return "Owned Elsewhere";
  }
}

function primaryActionColor(status: ItemStatus): AccentColor {
  switch (status) {
    case "available":
    case "updateAvailable":
    case "partiallyInstalled":
      return "green";
    case "installed":
    case "removed":
      return "red";
    case "conflict":
      return "amber";
    case "modified":
    case "sourceConflict":
      return "gray";
  }
}

function locatorKindLabel(kind: LocatorKind): string {
  switch (kind) {
    case "git":
      return "Git";
    case "artifact":
      return "Artifact";
  }
}

function sourceProvenance(state: AppState, source: SourceState): string | null {
  if (source.repositoryKey === null) {
    return null;
  }
  const repository = state.repositories.find((entry) => entry.repositoryKey === source.repositoryKey);
  return repository === undefined ? null : `From ${repository.name}`;
}

function componentLabel(kind: string): string {
  switch (kind) {
    case "skill":
      return "Skill";
    case "mcpServer":
      return "MCP";
    default:
      return kind;
  }
}

function installPreviewMessage(preview: InstallPreview, replacing: boolean): string {
  const compatibility = preview.compatibility.map((entry) => `${entry.targetId}/${entry.componentId}: ${entry.capability.level}`).join("\n");
  const resources = preview.resources.map((resource) => `${resource.shared ? "Shared " : ""}${resource.kind}: ${resource.identity}`).join("\n");
  const risks = preview.riskDetails.length === 0 ? "" : `\n\nExternal tool details:\n${preview.riskDetails.join("\n")}`;
  const warnings = preview.warnings.length === 0 ? "" : `\n\nReview:\n${preview.warnings.join("\n")}`;
  const replacement = replacing ? "\n\nExisting unmanaged resources will be backed up before replacement." : "";
  return `Trust tier ${String(preview.trustTier)}\n\nTargets and compatibility:\n${compatibility.length === 0 ? "Legacy explicit install" : compatibility}\n\nTransactional resources:\n${resources}${risks}${warnings}${replacement}`;
}

async function reviewInstall(item: CatalogItem, replacing: boolean): Promise<boolean | null> {
  const preview = await invokeParsed("preview_install_item", installPreviewSchema, { sourceId: item.sourceId, localId: item.localId });
  const approved = await confirm(installPreviewMessage(preview, replacing), {
    title: preview.requiresApproval ? "Approve external tools" : "Review installation",
    kind: preview.requiresApproval || replacing ? "warning" : "info",
    okLabel: preview.requiresApproval ? "Approve and Install" : replacing ? "Back Up and Replace" : "Install",
    cancelLabel: "Cancel"
  });
  return approved ? preview.requiresApproval : null;
}

function bulkLabels(action: BulkAction): Readonly<{ action: string; title: string; button: string; warning: string }> {
  switch (action) {
    case "install":
      return { action: "Install or update", title: "Install all", button: "Install", warning: "" };
    case "replace":
      return { action: "Replace", title: "Replace all", button: "Replace", warning: " Existing destinations will be backed up before replacement." };
    case "uninstall":
      return { action: "Uninstall", title: "Uninstall all", button: "Uninstall", warning: "" };
  }
}

async function reviewBulk(source: SourceState, action: BulkAction, plan: BulkPlan): Promise<boolean | null> {
  const eligible = plan.entries.filter((entry) => entry.willRun);
  const previews =
    action === "uninstall" ? [] : await Promise.all(eligible.map((entry) => invokeParsed("preview_install_item", installPreviewSchema, { sourceId: source.sourceId, localId: entry.localId })));
  const trustApproved = previews.some((preview) => preview.requiresApproval);
  const riskDetails = previews.flatMap((preview) => preview.riskDetails);
  const risks = riskDetails.length === 0 ? "" : `\n\nExternal tool details:\n${riskDetails.join("\n")}`;
  const labels = bulkLabels(action);
  const approved = await confirm(
    `${labels.action} ${String(eligible.length)} item${eligible.length === 1 ? "" : "s"} from ${source.name}?${labels.warning}${risks}\n\nAll changes use one transaction; any failure rolls back the complete batch.`,
    { title: labels.title, kind: trustApproved || action !== "install" ? "warning" : "info", okLabel: trustApproved ? "Approve and Install" : labels.button, cancelLabel: "Cancel" }
  );
  return approved ? trustApproved : null;
}

async function reviewAgentEnable(profile: AgentProfile): Promise<boolean> {
  const preview = await invokeParsed("preview_agent_enable", agentEnablePreviewSchema, { targetId: profile.targetId });
  const resources = preview.packages.flatMap((item) => item.resources);
  const warnings = preview.packages.flatMap((item) => item.warnings);
  const risks = preview.packages.flatMap((item) => item.riskDetails);
  const requiresApproval = preview.packages.some((item) => item.requiresApproval);
  const packageSummary =
    preview.packages.length === 0
      ? "No installed portable packages need reconciliation."
      : `${String(preview.packages.length)} installed portable package${preview.packages.length === 1 ? "" : "s"} will be reconciled across ${String(resources.length)} planned resource${resources.length === 1 ? "" : "s"}.`;
  const riskSummary = risks.length === 0 ? "" : `\n\nTier 3 details:\n${risks.join("\n")}`;
  const warningSummary = warnings.length === 0 ? "" : `\n\nWarnings:\n${warnings.join("\n")}`;
  return confirm(`${packageSummary}${riskSummary}${warningSummary}\n\n${profile.verificationGuidance} ${profile.reloadGuidance}`, {
    title: `Enable ${profile.displayName}`,
    kind: requiresApproval ? "warning" : "info",
    okLabel: requiresApproval ? "Approve and Enable" : "Enable",
    cancelLabel: "Cancel"
  });
}

async function reviewAgentDisable(profile: AgentProfile): Promise<boolean> {
  const cleanup = await invokeParsed("preview_agent_cleanup", targetCleanupPreviewSchema, { targetId: profile.targetId });
  const removed = cleanup.resourcesRemoved.length === 0 ? "No physical resources become unowned." : `Resources removed:\n${cleanup.resourcesRemoved.join("\n")}`;
  const retained = cleanup.resourcesRetained.length === 0 ? "" : `\n\nShared resources retained:\n${cleanup.resourcesRetained.join("\n")}`;
  return confirm(`${String(cleanup.bindingCount)} logical binding${cleanup.bindingCount === 1 ? "" : "s"} will be disabled.\n\n${removed}${retained}`, {
    title: `Disable ${profile.displayName}`,
    kind: "warning",
    okLabel: "Disable",
    cancelLabel: "Cancel"
  });
}

function supportsBulkAction(status: ItemStatus, action: BulkAction): boolean {
  switch (action) {
    case "install":
      return status === "available" || status === "updateAvailable" || status === "partiallyInstalled";
    case "replace":
      return status === "conflict";
    case "uninstall":
      return status === "installed" || status === "updateAvailable" || status === "partiallyInstalled";
  }
}

function hasEnabledAgent(profiles: readonly AgentProfile[]): boolean {
  return profiles.some((profile) => profile.enabled);
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

function AgentSetupNotice({ visible, onChoose }: Readonly<{ visible: boolean; onChoose: () => void }>): JSX.Element | null {
  if (!visible) {
    return null;
  }
  return (
    <Callout.Root className="app-callout notice" color="blue" role="status">
      <div className="callout-content">
        <Callout.Text>Select the agents you use. Portable skills and MCP servers cannot be installed or updated until at least one agent is enabled.</Callout.Text>
        <Button className="callout-action" size="1" onClick={onChoose}>
          Choose Agents
        </Button>
      </div>
    </Callout.Root>
  );
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

function ItemCard({
  item,
  sourceCommit,
  busy,
  onChange,
  onError
}: Readonly<{ item: CatalogItem; sourceCommit: string | null; busy: boolean; onChange: (item: CatalogItem) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const protectedItem = item.status === "modified" || item.status === "sourceConflict";
  const sourceBrowserUrl = sourceCommit === null || item.status === "removed" ? null : repositoryPathBrowserUrl(item.sourceUrl, sourceCommit, item.source, item.sourceIsDirectory);
  const destination = item.destination;
  return (
    <Card className="skill-card item-card">
      <div className="skill-copy">
        <div className="skill-title-row">
          <Heading as="h4" size="3">
            {item.name}
          </Heading>
          {item.components.map((component) => (
            <Badge key={`${component.kind}:${component.id}`} color="gray" variant="soft">
              {componentLabel(component.kind)}
            </Badge>
          ))}
          {item.status === "available" || item.status === "installed" ? null : <Badge color={statusColor(item.status)}>{statusLabel(item.status)}</Badge>}
          {item.manualInvocation ? <Badge color="blue">Manual Invocation</Badge> : null}
        </div>
        <Text as="p" color="gray" size="2">
          {item.description}
        </Text>
        <details className="item-details">
          <summary>Source and managed resource</summary>
          <dl>
            <dt>Source</dt>
            <dd>
              {sourceBrowserUrl === null ? (
                item.source
              ) : (
                <Button
                  className="source-path-link"
                  size="1"
                  variant="ghost"
                  onClick={() => {
                    openUrl(sourceBrowserUrl).catch((reason: unknown) => {
                      onError(errorText(reason));
                    });
                  }}
                >
                  {item.source}
                </Button>
              )}
            </dd>
            {destination === null ? null : (
              <>
                <dt>Primary resource</dt>
                <dd>
                  <Button
                    className="destination-link"
                    size="1"
                    variant="ghost"
                    onClick={() => {
                      revealItemInDir(destination).catch((reason: unknown) => {
                        onError(errorText(reason));
                      });
                    }}
                  >
                    {destination}
                  </Button>
                </dd>
              </>
            )}
          </dl>
        </details>
      </div>
      <div className="item-actions">
        <Button
          className="skill-action skill-action-primary"
          color={primaryActionColor(item.status)}
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
  onBulk: (source: SourceState, action: BulkAction) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const canInstall = items.some((item) => supportsBulkAction(item.status, "install"));
  const canReplace = items.some((item) => supportsBulkAction(item.status, "replace"));
  const canUninstall = items.some((item) => supportsBulkAction(item.status, "uninstall"));
  return (
    <section className="source-group">
      <div className="source-heading">
        <div>
          <div className="source-title-row">
            <Heading as="h3" size="4">
              <Button
                className="source-title-link"
                size="1"
                variant="ghost"
                onClick={() => {
                  openUrl(repositoryBrowserUrl(source.url) ?? source.url).catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                {source.name}
              </Button>
            </Heading>
            {source.refreshFailed ? <Badge color="red">Refresh failed</Badge> : null}
          </div>
          <Text as="p" color="gray" size="2">
            {source.description}
          </Text>
        </div>
        {canInstall || canReplace || canUninstall ? (
          <div className="source-group-actions">
            {canInstall ? (
              <Button
                size="1"
                variant="soft"
                color="green"
                disabled={allBusy}
                onClick={() => {
                  onBulk(source, "install").catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                Install All
              </Button>
            ) : null}
            {canReplace ? (
              <Button
                size="1"
                variant="soft"
                color="amber"
                disabled={allBusy}
                onClick={() => {
                  onBulk(source, "replace").catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                Replace All
              </Button>
            ) : null}
            {canUninstall ? (
              <Button
                size="1"
                variant="soft"
                color="red"
                disabled={allBusy}
                onClick={() => {
                  onBulk(source, "uninstall").catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                Uninstall All
              </Button>
            ) : null}
          </div>
        ) : null}
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
          <ItemCard key={item.id} item={item} sourceCommit={source.commit} busy={busyIds.has(item.id)} onChange={onItemChange} onError={onError} />
        ))}
        {items.length === 0 ? <Text color="gray">This source currently publishes no valid installs.</Text> : null}
      </div>
    </section>
  );
}

function LocatorKindToggle({ value, disabled, onChange }: Readonly<{ value: LocatorKind; disabled: boolean; onChange: (kind: LocatorKind) => void }>): JSX.Element {
  return (
    <div className="locator-kind" role="group" aria-label="Locator kind">
      {(["git", "artifact"] as const).map((kind) => (
        <Button
          key={kind}
          type="button"
          size="1"
          variant={value === kind ? "solid" : "soft"}
          disabled={disabled}
          onClick={() => {
            onChange(kind);
          }}
        >
          {locatorKindLabel(kind)}
        </Button>
      ))}
    </div>
  );
}

function locatorPlaceholder(kind: LocatorKind, catalog: boolean): string {
  if (kind === "artifact") {
    return catalog ? "https://host/repository/raw/catalog.json" : "https://host/repository/raw/source-latest.zip";
  }
  return catalog ? "https://github.com/owner/source-catalog" : "https://github.com/owner/repository";
}

function ManageSourcesDialog({
  open,
  state,
  adding,
  addingRepository,
  removing,
  removingRepositories,
  onOpenChange,
  onAdd,
  onAddListed,
  onAddRepository,
  onAddDefault,
  onRemove,
  onRemoveRepository,
  onError
}: Readonly<{
  open: boolean;
  state: AppState | null;
  adding: boolean;
  addingRepository: boolean;
  removing: ReadonlySet<string>;
  removingRepositories: ReadonlySet<string>;
  onOpenChange: (open: boolean) => void;
  onAdd: (kind: LocatorKind, url: string) => Promise<void>;
  onAddListed: (repository: RepositoryState, listed: ListedSource) => Promise<void>;
  onAddRepository: (kind: LocatorKind, url: string) => Promise<void>;
  onAddDefault: () => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
  onRemoveRepository: (repository: RepositoryState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const [sourceKind, setSourceKind] = useState<LocatorKind>("git");
  const [sourceUrl, setSourceUrl] = useState("");
  const [repositoryKind, setRepositoryKind] = useState<LocatorKind>("git");
  const [repositoryUrl, setRepositoryUrl] = useState("");

  function submitSource(event: SyntheticEvent<HTMLFormElement>): void {
    event.preventDefault();
    onAdd(sourceKind, sourceUrl)
      .then(() => {
        setSourceUrl("");
      })
      .catch((reason: unknown) => {
        onError(errorText(reason));
      });
  }

  function submitRepository(event: SyntheticEvent<HTMLFormElement>): void {
    event.preventDefault();
    onAddRepository(repositoryKind, repositoryUrl)
      .then(() => {
        setRepositoryUrl("");
      })
      .catch((reason: unknown) => {
        onError(errorText(reason));
      });
  }

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="720px">
        <Dialog.Title>Manage sources</Dialog.Title>
        <Dialog.Description>Add a source repository to browse catalogs, or add a source directly from Git or a raw HTTPS archive.</Dialog.Description>
        <section className="manage-section">
          <Heading as="h3" size="3">
            Source repositories
          </Heading>
          <Text as="p" color="gray" size="2">
            A catalog lists sources. Packages appear only after you add a listed source.
          </Text>
          <form className="source-form source-form-locator" onSubmit={submitRepository}>
            <LocatorKindToggle value={repositoryKind} disabled={addingRepository} onChange={setRepositoryKind} />
            <TextField.Root
              value={repositoryUrl}
              placeholder={locatorPlaceholder(repositoryKind, true)}
              onChange={(event) => {
                setRepositoryUrl(event.currentTarget.value);
              }}
            />
            <Button type="submit" disabled={addingRepository || repositoryUrl.trim().length === 0} loading={addingRepository}>
              Add repository
            </Button>
          </form>
          <div className="managed-sources">
            {state?.repositories.map((repository) => (
              <Card key={repository.repositoryKey} className="managed-source managed-repository">
                <div>
                  <div className="skill-title-row">
                    <Heading as="h3" size="2">
                      {repository.name}
                    </Heading>
                    <Badge color="gray" variant="soft">
                      {locatorKindLabel(repository.locatorKind)}
                    </Badge>
                    {repository.refreshFailed ? <Badge color="red">Refresh failed</Badge> : null}
                  </div>
                  <Text as="p" color="gray" size="1">
                    {repository.url}
                  </Text>
                  {repository.message === null ? null : (
                    <Text as="p" color="red" size="1">
                      {repository.message}
                    </Text>
                  )}
                  <ul className="listed-sources">
                    {repository.sources.map((listed) => (
                      <li key={`${listed.locatorKind}:${listed.url}`}>
                        <div>
                          <Text as="p" size="2">
                            {listed.name}
                          </Text>
                          <Text as="p" color="gray" size="1">
                            {listed.url}
                          </Text>
                        </div>
                        {listed.alreadyAdded ? (
                          <Badge color="green" variant="soft">
                            Added
                          </Badge>
                        ) : (
                          <Button
                            size="1"
                            disabled={adding}
                            onClick={() => {
                              onAddListed(repository, listed).catch((reason: unknown) => {
                                onError(errorText(reason));
                              });
                            }}
                          >
                            Add
                          </Button>
                        )}
                      </li>
                    ))}
                  </ul>
                </div>
                <Button
                  color="red"
                  size="1"
                  variant="soft"
                  loading={removingRepositories.has(repository.repositoryKey)}
                  disabled={removingRepositories.has(repository.repositoryKey)}
                  onClick={() => {
                    onRemoveRepository(repository).catch((reason: unknown) => {
                      onError(errorText(reason));
                    });
                  }}
                >
                  Remove
                </Button>
              </Card>
            ))}
          </div>
        </section>
        <section className="manage-section">
          <Heading as="h3" size="3">
            Sources
          </Heading>
          <Text as="p" color="gray" size="2">
            Add a Git repository or a raw HTTPS zip/tar that publishes skill-manager.json. Removing a source uninstalls its packages.
          </Text>
          <form className="source-form source-form-locator" onSubmit={submitSource}>
            <LocatorKindToggle value={sourceKind} disabled={adding} onChange={setSourceKind} />
            <TextField.Root
              value={sourceUrl}
              placeholder={locatorPlaceholder(sourceKind, false)}
              onChange={(event) => {
                setSourceUrl(event.currentTarget.value);
              }}
            />
            <Button type="submit" disabled={adding || sourceUrl.trim().length === 0} loading={adding}>
              Add source
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
                  <div className="skill-title-row">
                    <Heading as="h3" size="2">
                      {source.name}
                    </Heading>
                    <Badge color="gray" variant="soft">
                      {locatorKindLabel(source.locatorKind)}
                    </Badge>
                  </div>
                  <Text as="p" color="gray" size="1">
                    {source.url}
                  </Text>
                  {sourceProvenance(state, source) === null ? null : (
                    <Text as="p" color="gray" size="1">
                      {sourceProvenance(state, source)}
                    </Text>
                  )}
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
        </section>
        <div className="dialog-actions">
          <Dialog.Close>
            <Button variant="soft">Done</Button>
          </Dialog.Close>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function AgentProfilesDialog({
  open,
  profiles,
  busy,
  needsSelection,
  onOpenChange,
  onToggle,
  onError
}: Readonly<{
  open: boolean;
  profiles: readonly AgentProfile[];
  busy: ReadonlySet<TargetId>;
  needsSelection: boolean;
  onOpenChange: (open: boolean) => void;
  onToggle: (profile: AgentProfile) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="760px">
        <Dialog.Title>{needsSelection ? "Select the agents you use" : "Agents I use"}</Dialog.Title>
        <Dialog.Description>
          {needsSelection
            ? "Detected coding agents are selected automatically. Enable any others this machine should receive portable skills and MCP servers."
            : "Detected agents start enabled. Disable any you do not want Skill Manager to configure."}
        </Dialog.Description>
        <div className="agent-profiles">
          {profiles.map((profile) => (
            <Card key={profile.targetId} className="agent-profile">
              <div className="agent-profile-copy">
                <div className="skill-title-row">
                  <Heading as="h3" size="3">
                    {profile.displayName}
                  </Heading>
                  <Badge color={profile.enabled ? "green" : "gray"}>{profile.enabled ? "Enabled" : "Disabled"}</Badge>
                  <Badge color={profile.detected ? "blue" : "gray"} variant="soft">
                    {profile.detected ? "Detected" : "Not detected"}
                  </Badge>
                </div>
                <Text as="p" color="gray" size="2">
                  User scope · dialect {profile.dialectId}
                  {profile.detectedVersion === null ? "" : ` · ${profile.detectedVersion}`}
                </Text>
                {profile.detectionMessage === null ? null : (
                  <Text as="p" color="amber" size="1">
                    {profile.detectionMessage}
                  </Text>
                )}
                <Text as="p" color="gray" size="1">
                  Verify: {profile.verificationGuidance} Reload: {profile.reloadGuidance}
                </Text>
              </div>
              <Button
                color={profile.enabled ? "red" : "green"}
                variant="soft"
                loading={busy.has(profile.targetId)}
                disabled={busy.has(profile.targetId)}
                onClick={() => {
                  onToggle(profile).catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                {profile.enabled ? "Disable…" : "Enable"}
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
  const [addingRepository, setAddingRepository] = useState(false);
  const [sourceDialogOpen, setSourceDialogOpen] = useState(false);
  const [agentDialogOpen, setAgentDialogOpen] = useState(false);
  const [agentSetupPrompted, setAgentSetupPrompted] = useState(false);
  const [busyItems, setBusyItems] = useState<ReadonlySet<string>>(new Set());
  const [busySources, setBusySources] = useState<ReadonlySet<string>>(new Set());
  const [busyRepositories, setBusyRepositories] = useState<ReadonlySet<string>>(new Set());
  const [busyAgents, setBusyAgents] = useState<ReadonlySet<TargetId>>(new Set());

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

  useEffect(() => {
    if (state === null) {
      return;
    }
    if (hasEnabledAgent(state.agentProfiles)) {
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

  async function changeItem(item: CatalogItem): Promise<void> {
    if (item.status === "modified" || item.status === "sourceConflict") {
      return;
    }
    let command: "install_item" | "replace_item" | "uninstall_item";
    let trustApproved = false;
    if (item.status === "conflict") {
      command = "replace_item";
    } else if (item.status === "installed" || item.status === "removed") {
      const approved = await confirm(`Remove every unshared managed resource for ${item.name}? Shared resources still used by another agent will remain.`, {
        title: "Uninstall package",
        kind: "warning",
        okLabel: "Uninstall",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      command = "uninstall_item";
    } else {
      command = "install_item";
    }
    if (command !== "uninstall_item") {
      const reviewedTrust = await reviewInstall(item, command === "replace_item");
      if (reviewedTrust === null) {
        return;
      }
      trustApproved = reviewedTrust;
    }
    setBusyItems((current) => new Set(current).add(item.id));
    try {
      const outcome = await invokeParsed(command, operationOutcomeSchema, { sourceId: item.sourceId, localId: item.localId, trustApproved });
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
      const trustApproved = await reviewBulk(source, action, plan);
      if (trustApproved === null) {
        return;
      }
      const result = await invokeParsed("run_bulk_items", bulkResultSchema, { sourceId: source.sourceId, action, trustApproved });
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

  async function addSource(kind: LocatorKind, url: string, repositoryKey?: string): Promise<void> {
    setAdding(true);
    try {
      const prepared = await invokeParsed("prepare_source", preparedSourceSchema, { kind, url: url.trim(), repositoryKey: repositoryKey ?? null });
      const approved = await confirm(
        `${prepared.name} (${prepared.sourceId}) publishes ${String(prepared.itemCount)} valid install${prepared.itemCount === 1 ? "" : "s"} from ${locatorKindLabel(prepared.locatorKind)} at ${prepared.commit.slice(0, 12)}. Add this source?`,
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

  async function addListedSource(repository: RepositoryState, listed: ListedSource): Promise<void> {
    await addSource(listed.locatorKind, listed.url, repository.repositoryKey);
  }

  async function addRepository(kind: LocatorKind, url: string): Promise<void> {
    setAddingRepository(true);
    try {
      const prepared = await invokeParsed("prepare_source_repository", preparedRepositorySchema, { kind, url: url.trim() });
      const approved = await confirm(
        `${prepared.name} (${prepared.repositoryId}) lists ${String(prepared.sourceCount)} source${prepared.sourceCount === 1 ? "" : "s"} at ${prepared.revision.slice(0, 12)}. Add this catalog? Listed sources are not installed until you add them.`,
        { title: "Confirm source repository", kind: "info", okLabel: "Add repository", cancelLabel: "Cancel" }
      );
      if (!approved) {
        await invokeParsed("cancel_prepared_source_repository", unitSchema, { token: prepared.token });
        return;
      }
      applyState(await invokeParsed("confirm_source_repository", appStateSchema, { token: prepared.token }));
      setError(null);
    } finally {
      setAddingRepository(false);
    }
  }

  async function removeRepository(repository: RepositoryState): Promise<void> {
    setBusyRepositories((current) => new Set(current).add(repository.repositoryKey));
    try {
      const approved = await confirm(`Remove the ${repository.name} catalog? Opted-in sources stay configured.`, {
        title: "Remove source repository",
        kind: "warning",
        okLabel: "Remove",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
      applyState(await invokeParsed("remove_source_repository", appStateSchema, { repositoryKey: repository.repositoryKey }));
      setError(null);
    } finally {
      setBusyRepositories((current) => {
        const next = new Set(current);
        next.delete(repository.repositoryKey);
        return next;
      });
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

  async function toggleAgent(profile: AgentProfile): Promise<void> {
    setBusyAgents((current) => new Set(current).add(profile.targetId));
    try {
      let acknowledgeModifiedResources = false;
      const approved = profile.enabled ? await reviewAgentDisable(profile) : await reviewAgentEnable(profile);
      if (!approved) {
        return;
      }
      const trustApproved = !profile.enabled;
      let profiles: readonly AgentProfile[];
      try {
        profiles = await invokeParsed("set_agent_enabled", z.array(agentProfileSchema).readonly(), {
          targetId: profile.targetId,
          enabled: !profile.enabled,
          acknowledgeModifiedResources,
          trustApproved
        });
      } catch (reason) {
        if (!profile.enabled || !errorText(reason).includes("local changes")) {
          throw reason;
        }
        const approved = await confirm(`${errorText(reason)}\n\nBack up modified resources, then disable this agent?`, {
          title: "Modified managed resources",
          kind: "warning",
          okLabel: "Back Up and Disable",
          cancelLabel: "Cancel"
        });
        if (!approved) {
          return;
        }
        acknowledgeModifiedResources = true;
        profiles = await invokeParsed("set_agent_enabled", z.array(agentProfileSchema).readonly(), { targetId: profile.targetId, enabled: false, acknowledgeModifiedResources, trustApproved: false });
      }
      startTransition(() => {
        setState((current) => (current === null ? null : { ...current, agentProfiles: profiles }));
      });
      if (hasEnabledAgent(profiles)) {
        await synchronize();
      } else {
        await loadCached();
      }
    } finally {
      setBusyAgents((current) => {
        const next = new Set(current);
        next.delete(profile.targetId);
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
        </div>
        <div className="catalog-actions">
          <Button
            variant="soft"
            onClick={() => {
              setAgentDialogOpen(true);
            }}
          >
            Agents I Use
          </Button>
          <Button
            variant="soft"
            onClick={() => {
              setSourceDialogOpen(true);
            }}
          >
            Manage Sources
          </Button>
          <Button loading={syncing} disabled={syncing} onClick={() => void synchronize()}>
            Refresh
          </Button>
        </div>
      </header>
      <div className="sync-meta">
        <Text color="gray" size="1">
          Last checked: {checked}
        </Text>
      </div>
      <AgentSetupNotice
        visible={state !== null && !hasEnabledAgent(state.agentProfiles)}
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
        addingRepository={addingRepository}
        removing={busySources}
        removingRepositories={busyRepositories}
        onOpenChange={setSourceDialogOpen}
        onAdd={addSource}
        onAddListed={addListedSource}
        onAddRepository={addRepository}
        onAddDefault={addDefault}
        onRemove={removeSource}
        onRemoveRepository={removeRepository}
        onError={setError}
      />
      <AgentProfilesDialog
        open={agentDialogOpen}
        profiles={state?.agentProfiles ?? []}
        busy={busyAgents}
        needsSelection={state === null || !hasEnabledAgent(state.agentProfiles)}
        onOpenChange={setAgentDialogOpen}
        onToggle={toggleAgent}
        onError={setError}
      />
    </main>
  );
}
