import { startTransition, useCallback, useEffect, useMemo, useState } from "react";
import type { JSX, ReactNode } from "react";
import { Badge, Button, Callout, Card, Dialog, Heading, Spinner, Text } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { z } from "zod";
import "./App.css";

const SCHEDULED_SYNC_EVENT = "scheduled-sync";

const itemStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict", "sourceConflict", "partiallyInstalled"]);
const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
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
const componentSchema = z.strictObject({ id: z.string().min(1), kind: z.string().min(1), status: itemStatusSchema }).readonly();
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
    repositoryKey: z.string().min(1).nullable().default(null),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    commit: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    catalogErrors: z.array(catalogErrorSchema).readonly()
  })
  .readonly();
const listedSourceSchema = z
  .strictObject({ name: z.string().min(1), description: z.string().min(1), url: z.string().min(1), sourceId: z.string().min(2).nullable(), alreadyAdded: z.boolean() })
  .readonly();
const repositorySchema = z
  .strictObject({
    repositoryId: z.string().min(2),
    repositoryKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
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
    catalogMessage: z.string().min(1).nullable().default(null),
    repositories: z.array(repositorySchema).readonly().default([]),
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
    commit: z.string().min(1),
    itemCount: z.number().int().nonnegative()
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
type CatalogComponent = z.infer<typeof componentSchema>;
type ItemStatus = z.infer<typeof itemStatusSchema>;
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

function itemCommandArgs(item: CatalogItem, componentId: string | undefined, extra: Record<string, unknown>): Record<string, unknown> {
  return componentId === undefined ? { sourceId: item.sourceId, localId: item.localId, ...extra } : { sourceId: item.sourceId, localId: item.localId, componentId, ...extra };
}

function uninstallMessage(item: CatalogItem, component: CatalogComponent | undefined): string {
  if (component === undefined) {
    return `Remove every unshared managed resource for ${item.name}? Shared resources still used by another agent will remain.`;
  }
  return `Remove ${componentLabel(component.kind)} ${component.id} from ${item.name}? Other items in this package stay installed.`;
}

function commandForStatus(status: ItemStatus): "install_item" | "replace_item" | "uninstall_item" | null {
  if (status === "modified" || status === "sourceConflict") {
    return null;
  }
  if (status === "conflict") {
    return "replace_item";
  }
  if (status === "installed" || status === "removed") {
    return "uninstall_item";
  }
  return "install_item";
}

async function reviewInstall(item: CatalogItem, replacing: boolean, componentId?: string): Promise<boolean | null> {
  const preview = await invokeParsed("preview_install_item", installPreviewSchema, itemCommandArgs(item, componentId, {}));
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
}: Readonly<{ item: CatalogItem; sourceCommit: string | null; busy: boolean; onChange: (item: CatalogItem, componentId?: string) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const protectedItem = item.status === "modified" || item.status === "sourceConflict";
  const sourceBrowserUrl = sourceCommit === null || item.status === "removed" ? null : repositoryPathBrowserUrl(item.sourceUrl, sourceCommit, item.source, item.sourceIsDirectory);
  const destination = item.destination;
  const expandable = item.components.length > 1;
  return (
    <Card className={expandable ? "skill-card item-card item-card-expandable" : "skill-card item-card"}>
      <div className="skill-card-main">
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
            {packageActionLabel(item)}
          </Button>
        </div>
      </div>
      {expandable ? (
        <details className="component-list">
          <summary>
            {String(item.components.length)} items · {componentSummary(item.components)}
          </summary>
          <ul>
            {item.components.map((component) => (
              <li key={`${component.kind}:${component.id}`}>
                <ComponentRow component={component} busy={busy} protectedItem={protectedItem} onChange={() => onChange(item, component.id)} onError={onError} />
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </Card>
  );
}

function packageActionLabel(item: CatalogItem): string {
  if (item.status === "partiallyInstalled" && item.components.some((component) => component.status === "available")) {
    return "Install remaining";
  }
  return primaryActionLabel(item.status);
}

function componentSummary(components: readonly CatalogComponent[]): string {
  const skills = components.filter((component) => component.kind === "skill").length;
  const servers = components.filter((component) => component.kind === "mcpServer").length;
  const parts: string[] = [];
  if (skills > 0) {
    parts.push(`${String(skills)} skill${skills === 1 ? "" : "s"}`);
  }
  if (servers > 0) {
    parts.push(`${String(servers)} MCP`);
  }
  return parts.join(" · ");
}

function ComponentRow({
  component,
  busy,
  protectedItem,
  onChange,
  onError
}: Readonly<{ component: CatalogComponent; busy: boolean; protectedItem: boolean; onChange: () => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const blocked = protectedItem || component.status === "modified" || component.status === "sourceConflict";
  return (
    <div className="component-row">
      <div className="component-copy">
        <div className="skill-title-row">
          <Badge color="gray" variant="soft">
            {componentLabel(component.kind)}
          </Badge>
          <Text size="2">{component.id}</Text>
          {component.status === "available" || component.status === "installed" ? null : <Badge color={statusColor(component.status)}>{statusLabel(component.status)}</Badge>}
        </div>
      </div>
      <Button
        className="skill-action"
        size="1"
        color={primaryActionColor(component.status)}
        disabled={busy || blocked}
        loading={busy}
        onClick={() => {
          onChange().catch((reason: unknown) => {
            onError(errorText(reason));
          });
        }}
      >
        {primaryActionLabel(component.status)}
      </Button>
    </div>
  );
}

function SourceGroup({
  source,
  items,
  busyIds,
  allBusy,
  onItemChange,
  onBulk,
  onReset,
  onError
}: Readonly<{
  source: SourceState;
  items: readonly CatalogItem[];
  busyIds: ReadonlySet<string>;
  allBusy: boolean;
  onItemChange: (item: CatalogItem, componentId?: string) => Promise<void>;
  onBulk: (source: SourceState, action: BulkAction) => Promise<void>;
  onReset: (source: SourceState) => Promise<void>;
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
          <Button
            size="1"
            variant="soft"
            color="red"
            disabled={allBusy}
            onClick={() => {
              onReset(source).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Reset
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
          <ItemCard key={item.id} item={item} sourceCommit={source.commit} busy={busyIds.has(item.id)} onChange={onItemChange} onError={onError} />
        ))}
        {items.length === 0 ? <Text color="gray">This source currently publishes no valid installs.</Text> : null}
      </div>
    </section>
  );
}

function ListedSourceCard({ name, description, children }: Readonly<{ name: string; description: string; children: ReactNode }>): JSX.Element {
  return (
    <Card className="listed-source-card">
      <div className="listed-source-copy">
        <Text as="p" size="2">
          {name}
        </Text>
        <Text as="p" color="gray" size="2">
          {description}
        </Text>
      </div>
      <div className="listed-source-actions">{children}</div>
    </Card>
  );
}

function listedSourceKey(listed: ListedSource): string {
  return listed.sourceId ?? listed.url;
}

function sourceForListed(state: AppState, listed: ListedSource): SourceState | null {
  return state.sources.find((source) => listed.sourceId !== null && source.sourceId === listed.sourceId) ?? state.sources.find((source) => source.url === listed.url) ?? null;
}

function orphanSources(state: AppState): readonly SourceState[] {
  const listedUrls = new Set(state.repositories.flatMap((repository) => repository.sources.map((listed) => listed.url)));
  const listedIds = new Set(state.repositories.flatMap((repository) => repository.sources.flatMap((listed) => (listed.sourceId === null ? [] : [listed.sourceId]))));
  return state.sources.filter((source) => !listedUrls.has(source.url) && !listedIds.has(source.sourceId));
}

function ManageSourcesDialog({
  open,
  state,
  adding,
  removing,
  onOpenChange,
  onAddListed,
  onRemove,
  onError
}: Readonly<{
  open: boolean;
  state: AppState | null;
  adding: boolean;
  removing: ReadonlySet<string>;
  onOpenChange: (open: boolean) => void;
  onAddListed: (repository: RepositoryState, listed: ListedSource) => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const repository = state?.repositories[0] ?? null;
  const extras = state === null ? [] : orphanSources(state);

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="720px">
        <Dialog.Title>Manage sources</Dialog.Title>
        <Dialog.Description>Adding a source makes its skills and MCP servers available. Nothing is installed until you choose it.</Dialog.Description>
        {state?.catalogMessage === null || state?.catalogMessage === undefined ? null : (
          <Text as="p" color="red" size="2">
            {state.catalogMessage}
          </Text>
        )}
        {state === null || repository === null ? (
          <Text as="p" color="gray" size="2">
            The source catalog is not configured yet. Once the company catalog URL is set, sources will appear here.
          </Text>
        ) : (
          <section className="manage-section">
            <div className="skill-title-row">
              <Heading as="h3" size="3">
                {repository.name}
              </Heading>
              {repository.refreshFailed ? <Badge color="red">Refresh failed</Badge> : null}
            </div>
            <Text as="p" color="gray" size="2">
              {repository.description}
            </Text>
            {repository.message === null ? null : (
              <Text as="p" color="red" size="2">
                {repository.message}
              </Text>
            )}
            {repository.sources.length === 0 ? (
              <Text as="p" color="gray" size="2">
                This catalog does not list any sources yet.
              </Text>
            ) : (
              <ul className="listed-sources">
                {repository.sources.map((listed) => {
                  const added = listed.alreadyAdded ? sourceForListed(state, listed) : null;
                  return (
                    <li key={listedSourceKey(listed)}>
                      <ListedSourceCard name={listed.name} description={listed.description}>
                        {listed.alreadyAdded ? (
                          added === null ? null : (
                            <Button
                              color="red"
                              size="1"
                              variant="soft"
                              loading={removing.has(added.sourceId)}
                              disabled={removing.has(added.sourceId)}
                              onClick={() => {
                                onRemove(added).catch((reason: unknown) => {
                                  onError(errorText(reason));
                                });
                              }}
                            >
                              Remove
                            </Button>
                          )
                        ) : (
                          <Button
                            size="1"
                            disabled={adding}
                            loading={adding}
                            onClick={() => {
                              onAddListed(repository, listed).catch((reason: unknown) => {
                                onError(errorText(reason));
                              });
                            }}
                          >
                            Add
                          </Button>
                        )}
                      </ListedSourceCard>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        )}
        {extras.length === 0 ? null : (
          <section className="manage-section">
            <Heading as="h3" size="3">
              Other sources
            </Heading>
            <Text as="p" color="gray" size="2">
              These sources are no longer listed in the catalog.
            </Text>
            <ul className="listed-sources">
              {extras.map((source) => (
                <li key={source.sourceKey}>
                  <ListedSourceCard name={source.name} description={source.description}>
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
                      Remove
                    </Button>
                  </ListedSourceCard>
                </li>
              ))}
            </ul>
          </section>
        )}
        <Text as="p" color="gray" size="2">
          Need a new source? Ask the catalog owner to add it.
        </Text>
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
  const [sourceDialogOpen, setSourceDialogOpen] = useState(false);
  const [agentDialogOpen, setAgentDialogOpen] = useState(false);
  const [agentSetupPrompted, setAgentSetupPrompted] = useState(false);
  const [busyItems, setBusyItems] = useState<ReadonlySet<string>>(new Set());
  const [busySources, setBusySources] = useState<ReadonlySet<string>>(new Set());
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

  async function changeItem(item: CatalogItem, componentId?: string): Promise<void> {
    const component = componentId === undefined ? undefined : item.components.find((entry) => entry.id === componentId);
    if (componentId !== undefined && component === undefined) {
      return;
    }
    const command = commandForStatus(component?.status ?? item.status);
    if (command === null) {
      return;
    }
    let trustApproved = false;
    if (command === "uninstall_item") {
      const approved = await confirm(uninstallMessage(item, component), {
        title: component === undefined ? "Uninstall package" : "Uninstall item",
        kind: "warning",
        okLabel: "Uninstall",
        cancelLabel: "Cancel"
      });
      if (!approved) {
        return;
      }
    } else {
      const reviewedTrust = await reviewInstall(item, command === "replace_item", componentId);
      if (reviewedTrust === null) {
        return;
      }
      trustApproved = reviewedTrust;
    }
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

  async function resetSource(source: SourceState): Promise<void> {
    setBusySources((current) => new Set(current).add(source.sourceId));
    try {
      const approved = await confirm(
        `Uninstall every managed item from ${source.name}, including source conflicts and locally modified files? Ledger ownership for this source is wiped. The source stays added so you can reinstall.`,
        { title: "Reset source", kind: "warning", okLabel: "Reset", cancelLabel: "Cancel" }
      );
      if (!approved) {
        return;
      }
      let result;
      try {
        result = await invokeParsed("reset_source", bulkResultSchema, { sourceId: source.sourceId });
      } catch (reason) {
        const text = errorText(reason);
        if (text.toLowerCase().includes("reset_source") && text.toLowerCase().includes("not found")) {
          setError("Restart Skill Manager so it can load the Reset command, then try again.");
          return;
        }
        throw reason;
      }
      if (result.failures.length > 0) {
        setError(result.failures.map((failure) => `${failure.id}: ${failure.message}`).join("; "));
      } else {
        const count = result.completed.length;
        const backups = result.backupPaths.length === 0 ? "" : `\n\nBacked up leftover files at ${result.backupPaths.join(", ")}.`;
        await message(
          count === 0
            ? `No leftover ${source.name} ownership remained. Packages are available to install.${backups}`
            : `Removed ${String(count)} leftover install${count === 1 ? "" : "s"} from ${source.name}. Packages are available to install again.${backups}`,
          { title: "Source reset", kind: "info" }
        );
      }
      await synchronize();
    } finally {
      setBusySources((current) => {
        const next = new Set(current);
        next.delete(source.sourceId);
        return next;
      });
    }
  }

  async function addListedSource(repository: RepositoryState, listed: ListedSource): Promise<void> {
    setAdding(true);
    try {
      const prepared = await invokeParsed("prepare_source", preparedSourceSchema, { url: listed.url, repositoryKey: repository.repositoryKey });
      const approved = await confirm(`Add ${prepared.name}? Its packages will be available to install. Nothing is installed yet.`, {
        title: "Add source",
        kind: "info",
        okLabel: "Add",
        cancelLabel: "Cancel"
      });
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
              onReset={resetSource}
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
