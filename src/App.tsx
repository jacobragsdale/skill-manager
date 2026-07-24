import { startTransition, useCallback, useEffect, useRef, useState } from "react";
import type { JSX, ReactNode, SyntheticEvent } from "react";
import { Badge, Button, Callout, Card, Code, Dialog, Heading, Spinner, Text, TextField } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AnimatePresence, motion } from "motion/react";
import type { Transition } from "motion/react";
import { z } from "zod";
import "./App.css";

const AUTO_UPDATE_INTERVAL_MS = 15 * 60 * 1000;
const SCHEDULED_SYNC_EVENT = "scheduled-sync";
const ENTER_TRANSITION: Transition = { duration: 0.28, ease: [0.22, 1, 0.36, 1] };
const QUICK_TRANSITION: Transition = { duration: 0.18, ease: [0.22, 1, 0.36, 1] };

const skillStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "unmanagedMatch", "conflict", "sourceConflict"]);
const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
const itemKindSchema = z.enum(["skill", "rule"]);
const bundleStatusSchema = z.enum(["available", "partiallyInstalled", "installed", "updateAvailable", "needsAttention"]);

const installableSchema = z
  .strictObject({ sourceId: z.string().min(1), sourceName: z.string().min(1), sourceUrl: z.string().min(1), name: z.string().min(1), description: z.string().min(1), status: skillStatusSchema })
  .readonly();
const catalogErrorSchema = z.strictObject({ path: z.string().min(1), message: z.string().min(1) }).readonly();
const bundleMemberSchema = z.strictObject({ kind: itemKindSchema, name: z.string().min(1), status: skillStatusSchema }).readonly();
const bundleSchema = z
  .strictObject({
    sourceId: z.string().min(1),
    sourceName: z.string().min(1),
    sourceUrl: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    status: bundleStatusSchema,
    members: z.array(bundleMemberSchema).min(1).readonly()
  })
  .readonly();
const ruleTargetSchema = z
  .strictObject({ target: z.string().min(1), scope: z.string().min(1), path: z.string().min(1), active: z.boolean(), reloadRequired: z.string().min(1), message: z.string().min(1).nullable() })
  .readonly();

const sourceStateSchema = z
  .strictObject({
    id: z.string().min(1),
    name: z.string().min(1),
    url: z.string().min(1),
    builtIn: z.boolean(),
    status: sourceStatusSchema,
    message: z.string().min(1).nullable(),
    commit: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    catalogErrors: z.array(catalogErrorSchema).readonly()
  })
  .readonly();

const autoUpdateSkillSchema = z.strictObject({ sourceId: z.string().min(1), name: z.string().min(1) }).readonly();
const skillUpdateFailureSchema = z.strictObject({ sourceId: z.string().min(1), name: z.string().min(1), message: z.string().min(1) }).readonly();
const replaceUnmanagedResultSchema = z.strictObject({ backupPath: z.string().min(1) }).readonly();
const bulkPlanActionSchema = z.enum(["install", "update", "installed", "adopt", "conflict", "modified", "sourceConflict"]);
const bulkPlanEntrySchema = z.strictObject({ kind: itemKindSchema, name: z.string().min(1), action: bulkPlanActionSchema }).readonly();
const bulkPlanSchema = z
  .strictObject({ sourceId: z.string().min(1), bundleName: z.string().min(1).nullable(), hasConflicts: z.boolean(), entries: z.array(bulkPlanEntrySchema).readonly() })
  .readonly();
const bulkInstallResultSchema = z
  .strictObject({
    completed: z.array(bulkPlanEntrySchema).readonly(),
    failures: z.array(z.strictObject({ kind: itemKindSchema, name: z.string().min(1), message: z.string().min(1) }).readonly()).readonly()
  })
  .readonly();

const autoUpdateReportSchema = z
  .strictObject({
    updatedSkills: z.array(autoUpdateSkillSchema).readonly(),
    updatedRules: z.array(autoUpdateSkillSchema).readonly(),
    skippedModifiedSkills: z.array(autoUpdateSkillSchema).readonly(),
    skippedModifiedRules: z.array(autoUpdateSkillSchema).readonly(),
    skippedLegacySkills: z.array(autoUpdateSkillSchema).readonly(),
    failedSkills: z.array(skillUpdateFailureSchema).readonly(),
    failedRules: z.array(skillUpdateFailureSchema).readonly()
  })
  .readonly();

const appStateSchema = z
  .strictObject({
    installRoot: z.string().min(1),
    ruleInstallRoot: z.string().min(1),
    ruleTarget: ruleTargetSchema,
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    autoUpdateReport: autoUpdateReportSchema,
    sources: z.array(sourceStateSchema).readonly(),
    skills: z.array(installableSchema).readonly(),
    rules: z.array(installableSchema).readonly(),
    bundles: z.array(bundleSchema).readonly()
  })
  .readonly();

const cachedAppStateSchema = appStateSchema.nullable();
const scheduledSyncSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("updated"), state: appStateSchema }).readonly(),
  z.strictObject({ kind: z.literal("failed"), message: z.string().min(1) }).readonly()
]);
const checkedAtFormatter = new Intl.DateTimeFormat(undefined, { timeStyle: "medium" });

type SkillStatus = z.infer<typeof skillStatusSchema>;
type SourceStatus = z.infer<typeof sourceStatusSchema>;
type Installable = z.infer<typeof installableSchema>;
type ItemKind = z.infer<typeof itemKindSchema>;
type CatalogItem = Installable & Readonly<{ kind: ItemKind }>;
type Bundle = z.infer<typeof bundleSchema>;
type SourceState = z.infer<typeof sourceStateSchema>;
type AutoUpdateSkill = z.infer<typeof autoUpdateSkillSchema>;
type AutoUpdateReport = z.infer<typeof autoUpdateReportSchema>;
type AppState = z.infer<typeof appStateSchema>;
type CatalogGroup = Readonly<{ id: string; name: string; url: string; source: SourceState | null; items: readonly CatalogItem[] }>;
type CatalogFilter = "all" | ItemKind | "bundle";
const CATALOG_FILTERS: readonly CatalogFilter[] = ["all", "skill", "rule", "bundle"];
type ActionNotice =
  | Readonly<{ kind: "adopted"; sourceId: string; sourceName: string; name: string; itemKind: ItemKind }>
  | Readonly<{ kind: "replaced"; sourceId: string; sourceName: string; name: string; itemKind: ItemKind; backupPath: string }>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

function itemIdentity(item: CatalogItem): string {
  return `${item.kind}\u0000${item.sourceId}\u0000${item.name}`;
}

function filterLabel(filter: CatalogFilter): string {
  switch (filter) {
    case "all":
      return "All items";
    case "skill":
      return "Skills";
    case "rule":
      return "Rules";
    case "bundle":
      return "Bundles";
  }
}

function statusLabel(status: SkillStatus): string {
  switch (status) {
    case "available":
      return "Available";
    case "installed":
      return "Installed";
    case "updateAvailable":
      return "Update available";
    case "removed":
      return "Removed upstream";
    case "modified":
      return "Local changes";
    case "unmanagedMatch":
      return "Unmanaged match";
    case "conflict":
      return "Conflict";
    case "sourceConflict":
      return "Source conflict";
  }
}

function statusColor(status: SkillStatus): AccentColor {
  switch (status) {
    case "available":
      return "gray";
    case "installed":
      return "green";
    case "updateAvailable":
    case "unmanagedMatch":
      return "blue";
    case "removed":
    case "conflict":
    case "sourceConflict":
      return "amber";
    case "modified":
      return "red";
  }
}

function sourceStatusLabel(status: SourceStatus): string {
  switch (status) {
    case "fresh":
      return "Fresh";
    case "cached":
      return "Cached";
    case "error":
      return "Error";
  }
}

function sourceStatusColor(status: SourceStatus): AccentColor {
  switch (status) {
    case "fresh":
      return "green";
    case "cached":
      return "amber";
    case "error":
      return "red";
  }
}

function actionLabel(status: SkillStatus, busy: boolean): string {
  if (busy) {
    return "Working…";
  }

  switch (status) {
    case "available":
      return "Install";
    case "installed":
    case "removed":
      return "Uninstall";
    case "updateAvailable":
      return "Update";
    case "modified":
      return "Protected";
    case "unmanagedMatch":
      return "Manage";
    case "conflict":
      return "Replace…";
    case "sourceConflict":
      return "Installed elsewhere";
  }
}

function displaySourceName(sourceId: string, sources: readonly SourceState[]): string {
  return sources.find((source) => source.id === sourceId)?.name ?? "removed source";
}

function reportSkillLabel(skill: AutoUpdateSkill, sources: readonly SourceState[]): string {
  return `${skill.name} (${displaySourceName(skill.sourceId, sources)})`;
}

function autoUpdateMessage(report: AutoUpdateReport, sources: readonly SourceState[]): string | null {
  const messages: string[] = [];

  if (report.updatedSkills.length > 0) {
    messages.push(`Automatically updated skills ${report.updatedSkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`);
  }
  if (report.updatedRules.length > 0) {
    messages.push(`Automatically updated rules ${report.updatedRules.map((rule) => reportSkillLabel(rule, sources)).join(", ")}.`);
  }
  if (report.skippedModifiedSkills.length > 0) {
    messages.push(`Protected local skill changes in ${report.skippedModifiedSkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`);
  }
  if (report.skippedModifiedRules.length > 0) {
    messages.push(`Protected local rule changes in ${report.skippedModifiedRules.map((rule) => reportSkillLabel(rule, sources)).join(", ")}.`);
  }
  if (report.skippedLegacySkills.length > 0) {
    messages.push(
      `Legacy installs require one manual update before automatic updates can manage them safely: ${report.skippedLegacySkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`
    );
  }
  if (report.failedSkills.length > 0) {
    messages.push(`Automatic skill update failed for ${report.failedSkills.map((failure) => `${reportSkillLabel(failure, sources)}: ${failure.message}`).join("; ")}.`);
  }
  if (report.failedRules.length > 0) {
    messages.push(`Automatic rule update failed for ${report.failedRules.map((failure) => `${reportSkillLabel(failure, sources)}: ${failure.message}`).join("; ")}.`);
  }

  return messages.length === 0 ? null : messages.join(" ");
}

function catalogSummary(state: AppState | null): string {
  if (state === null) {
    return "Loading catalog sources…";
  }

  const skillCount = state.skills.filter((skill) => skill.status !== "removed").length;
  const ruleCount = state.rules.filter((rule) => rule.status !== "removed").length;
  const bundleCount = state.bundles.length;
  const sourceCount = state.sources.length;
  const cachedCount = state.sources.filter((source) => source.status === "cached").length;
  const errorCount = state.sources.filter((source) => source.status === "error").length;
  const checkedAt = checkedAtFormatter.format(new Date(state.checkedAtEpochSeconds * 1000));
  const cached = cachedCount === 0 ? "" : ` · ${String(cachedCount)} cached`;
  const errors = errorCount === 0 ? "" : ` · ${String(errorCount)} failed`;
  return `${String(skillCount)} skill${skillCount === 1 ? "" : "s"} · ${String(ruleCount)} rule${ruleCount === 1 ? "" : "s"} · ${String(bundleCount)} bundle${bundleCount === 1 ? "" : "s"} from ${String(sourceCount)} source${sourceCount === 1 ? "" : "s"} · checked ${checkedAt}${cached}${errors}`;
}

function sourceCheckedAt(source: SourceState): string {
  if (source.checkedAtEpochSeconds === 0) {
    return "Not checked yet";
  }
  return `Checked ${checkedAtFormatter.format(new Date(source.checkedAtEpochSeconds * 1000))}`;
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

function RepositoryUrlLink({ url, className, onError }: Readonly<{ url: string; className: string; onError: (message: string) => void }>): JSX.Element {
  const browserUrl = repositoryBrowserUrl(url);
  const urlText = (
    <Code className={className} color="gray" size="1" variant="ghost">
      {url}
    </Code>
  );

  if (browserUrl === null) {
    return urlText;
  }

  return (
    <a
      className="repository-url-link"
      href={browserUrl}
      title="Open repository in browser"
      onClick={(event) => {
        event.preventDefault();
        openUrl(browserUrl).catch((reason: unknown) => {
          onError(`Could not open the repository: ${String(reason)}`);
        });
      }}
    >
      {urlText}
    </a>
  );
}

function stateAutoUpdateMessage(state: AppState | null): string | null {
  return state === null ? null : autoUpdateMessage(state.autoUpdateReport, state.sources);
}

function sourceCount(state: AppState | null): number {
  return state === null ? 0 : state.sources.length;
}

function effectiveBusySkill(busySkill: string | null, addingSource: boolean, busySourceId: string | null): string | null {
  return addingSource || busySourceId !== null ? "source-mutation" : busySkill;
}

function catalogItems(state: AppState, filter: CatalogFilter): readonly CatalogItem[] {
  const skills = filter === "all" || filter === "skill" ? state.skills.map((skill): CatalogItem => ({ ...skill, kind: "skill" })) : [];
  const rules = filter === "all" || filter === "rule" ? state.rules.map((rule): CatalogItem => ({ ...rule, kind: "rule" })) : [];
  return [...skills, ...rules];
}

function sourceGroups(state: AppState, filter: CatalogFilter): readonly CatalogGroup[] {
  const items = catalogItems(state, filter);
  const activeGroups = state.sources.map((source): CatalogGroup => {
    return { id: source.id, name: source.name, url: source.url, source, items: items.filter((item) => item.sourceId === source.id) };
  });
  const knownIds = new Set(state.sources.map((source) => source.id));
  const orphanItems = items.filter((item) => !knownIds.has(item.sourceId));
  const orphanGroups = orphanItems
    .filter((item, index, allItems) => allItems.findIndex((candidate) => candidate.sourceId === item.sourceId) === index)
    .map((item): CatalogGroup => {
      return { id: item.sourceId, name: item.sourceName, url: item.sourceUrl, source: null, items: orphanItems.filter((candidate) => candidate.sourceId === item.sourceId) };
    });
  return [...activeGroups, ...orphanGroups];
}

function changedInstallables(entries: readonly Installable[], selectedItem: CatalogItem): readonly Installable[] {
  const uninstalling = selectedItem.status === "installed" || selectedItem.status === "removed";
  if (selectedItem.status === "removed") {
    return entries
      .filter((entry) => entry.sourceId !== selectedItem.sourceId || entry.name !== selectedItem.name)
      .map((entry) => (entry.name === selectedItem.name && entry.status === "sourceConflict" ? { ...entry, status: "available" } : entry));
  }
  return entries.map((entry) => {
    if (entry.sourceId === selectedItem.sourceId && entry.name === selectedItem.name) {
      return { ...entry, status: uninstalling ? "available" : "installed" };
    }
    if (entry.name === selectedItem.name && entry.status === "sourceConflict" && uninstalling) {
      return { ...entry, status: "available" };
    }
    if (entry.name === selectedItem.name && !uninstalling) {
      return { ...entry, status: "sourceConflict" };
    }
    return entry;
  });
}

function stateAfterInstallationChange(state: AppState, selectedItem: CatalogItem): AppState {
  return selectedItem.kind === "skill" ? { ...state, skills: changedInstallables(state.skills, selectedItem) } : { ...state, rules: changedInstallables(state.rules, selectedItem) };
}

function AppCallout({ color, role, children, action }: Readonly<{ color: "amber" | "green" | "red"; role: "alert" | "status"; children: ReactNode; action?: ReactNode }>): JSX.Element {
  return (
    <motion.div
      className="callout-motion"
      layout
      initial={{ opacity: 0, y: -8, scale: 0.995 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, y: -6, scale: 0.995 }}
      transition={QUICK_TRANSITION}
    >
      <Callout.Root className="app-callout" color={color} role={role} size="1" variant="surface">
        <div className="callout-content">
          <Callout.Text>{children}</Callout.Text>
          {action}
        </div>
      </Callout.Root>
    </motion.div>
  );
}

function ActionNoticeMessage({ notice, onDismiss }: Readonly<{ notice: ActionNotice; onDismiss: () => void }>): JSX.Element {
  return (
    <AppCallout
      color="green"
      role="status"
      action={
        <Button className="callout-action" type="button" color="green" size="1" variant="ghost" onClick={onDismiss}>
          Dismiss
        </Button>
      }
    >
      {notice.kind === "adopted" ? (
        <span>
          {notice.itemKind}:{notice.name} from {notice.sourceName} is now managed by Skill Manager.
        </span>
      ) : (
        <span>
          Replaced {notice.itemKind}:{notice.name} from {notice.sourceName}. The original remains at <Code variant="ghost">{notice.backupPath}</Code>.
        </span>
      )}
    </AppCallout>
  );
}

function NoticeStack({
  error,
  updateMessage,
  actionNotice,
  onRetry,
  onDismissAction
}: Readonly<{ error: string | null; updateMessage: string | null; actionNotice: ActionNotice | null; onRetry: () => void; onDismissAction: () => void }>): JSX.Element {
  return (
    <div className="notice-stack">
      <AnimatePresence initial={false} mode="popLayout">
        {error !== null && (
          <AppCallout
            key="error"
            color="red"
            role="alert"
            action={
              <Button className="callout-action" type="button" color="red" size="1" variant="ghost" onClick={onRetry}>
                Try again
              </Button>
            }
          >
            {error}
          </AppCallout>
        )}

        {updateMessage !== null && (
          <AppCallout key="update-message" color="amber" role="status">
            {updateMessage}
          </AppCallout>
        )}

        {actionNotice !== null && <ActionNoticeMessage key={`${actionNotice.kind}-${actionNotice.sourceId}-${actionNotice.name}`} notice={actionNotice} onDismiss={onDismissAction} />}
      </AnimatePresence>
    </div>
  );
}

function RefreshIcon(): JSX.Element {
  return (
    <svg className="refresh-icon" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M13.25 5.75A5.75 5.75 0 1 0 13.4 9" />
      <path d="M10.25 5.75h3v-3" />
    </svg>
  );
}

function SourcesIcon(): JSX.Element {
  return (
    <svg className="sources-icon" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <circle cx="4" cy="4" r="2" />
      <circle cx="12" cy="4" r="2" />
      <circle cx="8" cy="12" r="2" />
      <path d="m5.7 5 1.4 4.8M10.3 5 8.9 9.8M6 4h4" />
    </svg>
  );
}

function SourceMessage({ source }: Readonly<{ source: SourceState | null }>): JSX.Element | null {
  if (source === null) {
    return (
      <Callout.Root className="source-callout" color="amber" role="status" size="1" variant="surface">
        <Callout.Text>This source was removed. Installed skills and rules remain available for safe uninstall.</Callout.Text>
      </Callout.Root>
    );
  }
  if (source.message === null && source.catalogErrors.length === 0) {
    return null;
  }
  return (
    <Callout.Root className="source-callout" color={source.status === "error" ? "red" : "amber"} role={source.status === "error" ? "alert" : "status"} size="1" variant="surface">
      <Callout.Text>
        {source.message}
        {source.catalogErrors.map((error) => (
          <span key={error.path}>
            {source.message === null ? "" : " "}
            {error.path}: {error.message}
          </span>
        ))}
      </Callout.Text>
    </Callout.Root>
  );
}

function ItemCard({
  item,
  busySkill,
  index,
  onChangeInstallation,
  onError
}: Readonly<{ item: CatalogItem; busySkill: string | null; index: number; onChangeInstallation: (item: CatalogItem) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const identity = itemIdentity(item);
  const busy = busySkill === identity;
  const installed = item.status === "installed";
  const removed = item.status === "removed";
  const blocked = item.status === "modified" || item.status === "sourceConflict";
  const uninstall = installed || removed;
  const conflict = item.status === "conflict";

  return (
    <motion.article
      className="skill-card-motion"
      key={identity}
      layout
      initial={{ opacity: 0, y: 10, scale: 0.99 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, y: -8, scale: 0.99 }}
      transition={{ ...ENTER_TRANSITION, delay: Math.min(index * 0.035, 0.24) }}
    >
      <Card className="skill-card" size="2" variant="surface">
        <div className="skill-copy">
          <div className="skill-title-row">
            <Heading as="h4" size="3" weight="bold">
              {item.name}
            </Heading>
            <Badge color={item.kind === "skill" ? "blue" : "amber"} radius="full" size="1" variant="outline">
              {item.kind}
            </Badge>
            <AnimatePresence initial={false} mode="wait">
              <motion.span
                className="status-motion"
                key={item.status}
                initial={{ opacity: 0, scale: 0.94 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.94 }}
                transition={QUICK_TRANSITION}
              >
                <Badge color={statusColor(item.status)} highContrast radius="full" size="1" variant="soft">
                  {statusLabel(item.status)}
                </Badge>
              </motion.span>
            </AnimatePresence>
          </div>
          <Text as="p" color="gray" size="2">
            {item.description}
          </Text>
        </div>
        <Button
          className={`skill-action ${conflict ? "skill-action-warning" : uninstall ? "skill-action-destructive" : "skill-action-primary"}`}
          type="button"
          color={conflict ? "amber" : uninstall ? "red" : "blue"}
          highContrast={!uninstall && !conflict}
          loading={busy}
          size="2"
          variant={uninstall || conflict ? "soft" : "solid"}
          disabled={busySkill !== null || blocked}
          onClick={() => {
            onChangeInstallation(item).catch((reason: unknown) => {
              onError(String(reason));
            });
          }}
        >
          {actionLabel(item.status, busy)}
        </Button>
      </Card>
    </motion.article>
  );
}

function CatalogGroupSection({
  group,
  busySkill,
  startIndex,
  onChangeInstallation,
  onInstallAll,
  onError
}: Readonly<{
  group: CatalogGroup;
  busySkill: string | null;
  startIndex: number;
  onChangeInstallation: (item: CatalogItem) => Promise<void>;
  onInstallAll: (sourceId: string, bundleName: string | null) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  return (
    <section className="source-group" aria-labelledby={`source-heading-${group.id}`}>
      <div className="source-group-heading">
        <div className="source-group-copy">
          <div className="source-title-row">
            <Heading id={`source-heading-${group.id}`} as="h3" size="3" weight="bold">
              {group.name}
            </Heading>
            {group.source === null ? (
              <Badge color="amber" highContrast radius="full" size="1" variant="soft">
                Source removed
              </Badge>
            ) : (
              <Badge color={sourceStatusColor(group.source.status)} highContrast radius="full" size="1" variant="soft">
                {sourceStatusLabel(group.source.status)}
              </Badge>
            )}
          </div>
          <RepositoryUrlLink url={group.url} className="source-url" onError={onError} />
        </div>
        <div className="source-group-actions">
          <Text as="span" color="gray" size="1">
            {String(group.items.length)} item{group.items.length === 1 ? "" : "s"}
          </Text>
          {group.source !== null && group.items.length > 0 && (
            <Button
              type="button"
              color="blue"
              size="1"
              variant="soft"
              disabled={busySkill !== null}
              onClick={() => {
                onInstallAll(group.id, null).catch((reason: unknown) => {
                  onError(String(reason));
                });
              }}
            >
              Install all
            </Button>
          )}
        </div>
      </div>

      <SourceMessage source={group.source} />

      <div className="source-skill-list">
        {group.items.map((item, index) => (
          <ItemCard key={itemIdentity(item)} item={item} busySkill={busySkill} index={startIndex + index} onChangeInstallation={onChangeInstallation} onError={onError} />
        ))}
        {group.items.length === 0 && (
          <Card className="empty-source-card" size="2" variant="surface">
            <Text as="p" color="gray" size="2">
              No matching items found in this source.
            </Text>
          </Card>
        )}
      </div>
    </section>
  );
}

function CatalogList({
  state,
  busySkill,
  filter,
  onChangeInstallation,
  onInstallAll,
  onError
}: Readonly<{
  state: AppState | null;
  busySkill: string | null;
  filter: CatalogFilter;
  onChangeInstallation: (item: CatalogItem) => Promise<void>;
  onInstallAll: (sourceId: string, bundleName: string | null) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  if (state === null) {
    return (
      <div className="skill-list">
        <Card className="loading-card" size="2" variant="surface">
          <Spinner size="2" />
          <div>
            <Text as="p" size="2" weight="medium">
              Loading catalog sources…
            </Text>
            <Text as="p" color="gray" size="1">
              Checking each repository for the latest skills, rules, and bundles.
            </Text>
          </div>
        </Card>
      </div>
    );
  }

  const groups = sourceGroups(state, filter);
  let startIndex = 0;

  return (
    <div className="skill-list">
      <AnimatePresence initial mode="popLayout">
        {groups.map((group) => {
          const groupStartIndex = startIndex;
          startIndex += group.items.length;
          return (
            <motion.div className="source-group-motion" key={group.id} layout exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
              <CatalogGroupSection group={group} busySkill={busySkill} startIndex={groupStartIndex} onChangeInstallation={onChangeInstallation} onInstallAll={onInstallAll} onError={onError} />
            </motion.div>
          );
        })}

        {groups.length === 0 && (
          <motion.div key="empty" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
            <Card className="loading-card" size="2" variant="surface">
              <Text as="p" color="gray" size="2">
                No catalog sources are configured.
              </Text>
            </Card>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function bundleStatusLabel(status: Bundle["status"]): string {
  switch (status) {
    case "available":
      return "Available";
    case "partiallyInstalled":
      return "Partially installed";
    case "installed":
      return "Installed";
    case "updateAvailable":
      return "Update available";
    case "needsAttention":
      return "Needs attention";
  }
}

function bundleStatusColor(status: Bundle["status"]): AccentColor {
  switch (status) {
    case "available":
      return "gray";
    case "partiallyInstalled":
    case "updateAvailable":
      return "amber";
    case "installed":
      return "green";
    case "needsAttention":
      return "red";
  }
}

function bundleItem(state: AppState, bundle: Bundle, kind: ItemKind, name: string): CatalogItem | null {
  const entries = kind === "skill" ? state.skills : state.rules;
  const entry = entries.find((candidate) => candidate.sourceId === bundle.sourceId && candidate.name === name);
  return entry === undefined ? null : { ...entry, kind };
}

function BundleList({
  state,
  busySkill,
  onChangeInstallation,
  onInstallAll,
  onError
}: Readonly<{
  state: AppState | null;
  busySkill: string | null;
  onChangeInstallation: (item: CatalogItem) => Promise<void>;
  onInstallAll: (sourceId: string, bundleName: string | null) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  if (state === null) {
    return (
      <Card className="loading-card" size="2" variant="surface">
        <Spinner size="2" />
        <Text as="p" size="2">
          Loading bundles…
        </Text>
      </Card>
    );
  }
  return (
    <div className="skill-list">
      {state.bundles.map((bundle, index) => (
        <motion.article
          className="skill-card-motion"
          key={`${bundle.sourceId}\u0000${bundle.name}`}
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ ...ENTER_TRANSITION, delay: Math.min(index * 0.035, 0.24) }}
        >
          <Card className="bundle-card" size="2" variant="surface">
            <div className="bundle-heading">
              <div>
                <div className="skill-title-row">
                  <Heading as="h3" size="3">
                    {bundle.name}
                  </Heading>
                  <Badge color={bundleStatusColor(bundle.status)} highContrast radius="full" size="1" variant="soft">
                    {bundleStatusLabel(bundle.status)}
                  </Badge>
                </div>
                <Text as="p" color="gray" size="2">
                  {bundle.description}
                </Text>
                <Text as="p" color="gray" size="1">
                  {bundle.sourceName}
                </Text>
              </div>
              <Button
                type="button"
                color="blue"
                size="2"
                disabled={busySkill !== null}
                onClick={() => {
                  onInstallAll(bundle.sourceId, bundle.name).catch((reason: unknown) => {
                    onError(String(reason));
                  });
                }}
              >
                Install all
              </Button>
            </div>
            <div className="bundle-members">
              {bundle.members.map((member) => {
                const item = bundleItem(state, bundle, member.kind, member.name);
                const identity = item === null ? `${member.kind}-${member.name}` : itemIdentity(item);
                const blocked = item === null || item.status === "modified" || item.status === "sourceConflict";
                return (
                  <div className="bundle-member" key={identity}>
                    <div className="bundle-member-copy">
                      <Badge color={member.kind === "skill" ? "blue" : "amber"} size="1" variant="outline">
                        {member.kind}
                      </Badge>
                      <Text as="span" size="2" weight="medium">
                        {member.name}
                      </Text>
                      <Badge color={statusColor(member.status)} size="1" variant="soft">
                        {statusLabel(member.status)}
                      </Badge>
                    </div>
                    {item !== null && (
                      <Button
                        type="button"
                        color={item.status === "installed" || item.status === "removed" ? "red" : "blue"}
                        size="1"
                        variant="soft"
                        disabled={busySkill !== null || blocked}
                        onClick={() => {
                          onChangeInstallation(item).catch((reason: unknown) => {
                            onError(String(reason));
                          });
                        }}
                      >
                        {actionLabel(item.status, busySkill === identity)}
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
          </Card>
        </motion.article>
      ))}
      {state.bundles.length === 0 && (
        <Card className="loading-card" size="2" variant="surface">
          <Text as="p" color="gray" size="2">
            No bundles are published by the configured sources.
          </Text>
        </Card>
      )}
    </div>
  );
}

function SourceListItem({
  source,
  busySourceId,
  sourceMutationBusy,
  onRemove,
  onError
}: Readonly<{ source: SourceState; busySourceId: string | null; sourceMutationBusy: boolean; onRemove: (source: SourceState) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const busy = busySourceId === source.id;
  return (
    <Card className="source-item" size="2" variant="surface">
      <div className="source-item-topline">
        <div className="source-item-name">
          <Text as="span" size="2" weight="bold">
            {source.name}
          </Text>
          {source.builtIn && (
            <Badge color="gray" radius="full" size="1" variant="soft">
              Recommended
            </Badge>
          )}
          <Badge color={sourceStatusColor(source.status)} highContrast radius="full" size="1" variant="soft">
            {sourceStatusLabel(source.status)}
          </Badge>
        </div>
        <Button
          type="button"
          color="red"
          loading={busy}
          size="1"
          variant="soft"
          disabled={sourceMutationBusy}
          aria-label={`Remove ${source.name}`}
          onClick={() => {
            onRemove(source).catch((reason: unknown) => {
              onError(String(reason));
            });
          }}
        >
          Remove
        </Button>
      </div>
      <RepositoryUrlLink url={source.url} className="source-item-url" onError={onError} />
      <div className="source-item-meta">
        <Text as="span" color="gray" size="1">
          {sourceCheckedAt(source)}
        </Text>
        {source.commit !== null && (
          <Code color="gray" size="1" variant="ghost">
            {source.commit.slice(0, 7)}
          </Code>
        )}
      </div>
      {source.message !== null && (
        <Text className={source.status === "error" ? "source-item-error" : "source-item-message"} as="p" color={source.status === "error" ? "red" : "gray"} size="1">
          {source.message}
        </Text>
      )}
      {source.catalogErrors.map((error) => (
        <Text className="source-item-message" as="p" color="amber" size="1" key={error.path}>
          {error.path}: {error.message}
        </Text>
      ))}
    </Card>
  );
}

function SourcesDialog({
  open,
  sources,
  sourceUrl,
  sourceError,
  sourceMutationError,
  busySourceId,
  addingSource,
  disabled,
  onOpenChange,
  onSourceUrlChange,
  onAdd,
  onAddDefault,
  onRemove,
  onError
}: Readonly<{
  open: boolean;
  sources: readonly SourceState[];
  sourceUrl: string;
  sourceError: string | null;
  sourceMutationError: string | null;
  busySourceId: string | null;
  addingSource: boolean;
  disabled: boolean;
  onOpenChange: (open: boolean) => void;
  onSourceUrlChange: (value: string) => void;
  onAdd: (event: SyntheticEvent<HTMLFormElement>) => void;
  onAddDefault: () => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const sourceMutationBusy = addingSource || busySourceId !== null;
  return (
    <Dialog.Root
      open={open}
      onOpenChange={(nextOpen) => {
        if (nextOpen || !sourceMutationBusy) {
          onOpenChange(nextOpen);
        }
      }}
    >
      <Dialog.Trigger>
        <Button className="sources-button" type="button" color="gray" highContrast size="2" variant="surface" disabled={disabled}>
          <SourcesIcon />
          Sources
        </Button>
      </Dialog.Trigger>
      <Dialog.Content className="sources-dialog" maxWidth="620px">
        <Dialog.Title>Manage sources</Dialog.Title>
        <Dialog.Description size="2">
          Add Git repositories with optional <Code variant="ghost">skills/</Code>, <Code variant="ghost">rules/</Code>, and <Code variant="ghost">bundles/</Code> directories.
        </Dialog.Description>

        {sourceMutationError !== null && (
          <Text className="source-dialog-error" as="p" color="red" role="alert" size="1">
            {sourceMutationError}
          </Text>
        )}

        <div className="source-manager-list">
          {sources.map((source) => (
            <SourceListItem key={source.id} source={source} busySourceId={busySourceId} sourceMutationBusy={sourceMutationBusy} onRemove={onRemove} onError={onError} />
          ))}
        </div>

        {!sources.some((source) => source.builtIn) && (
          <Button
            type="button"
            color="gray"
            variant="soft"
            disabled={sourceMutationBusy}
            onClick={() => {
              onAddDefault().catch((reason: unknown) => {
                onError(String(reason));
              });
            }}
          >
            Add default skillbook source
          </Button>
        )}

        <form className="add-source-form" onSubmit={onAdd}>
          <label htmlFor="source-url">
            <Text as="span" size="2" weight="medium">
              Git repository URL
            </Text>
          </label>
          <div className="add-source-controls">
            <TextField.Root
              id="source-url"
              name="sourceUrl"
              type="text"
              autoCapitalize="none"
              autoComplete="url"
              spellCheck={false}
              placeholder="https://github.com/you/skills.git"
              value={sourceUrl}
              disabled={sourceMutationBusy}
              aria-invalid={sourceError !== null}
              aria-describedby={sourceError === null ? undefined : "source-url-error"}
              onChange={(event) => {
                onSourceUrlChange(event.currentTarget.value);
              }}
            />
            <Button type="submit" color="blue" highContrast loading={addingSource} disabled={sourceUrl.trim().length === 0 || sourceMutationBusy}>
              Add source
            </Button>
          </div>
          {sourceError !== null && (
            <Text id="source-url-error" className="source-form-error" as="p" color="red" role="alert" size="1">
              {sourceError}
            </Text>
          )}
        </form>

        <div className="dialog-actions">
          <Dialog.Close>
            <Button type="button" color="gray" variant="soft" disabled={sourceMutationBusy}>
              Done
            </Button>
          </Dialog.Close>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function CatalogFilters({ filter, onChange }: Readonly<{ filter: CatalogFilter; onChange: (filter: CatalogFilter) => void }>): JSX.Element {
  return (
    <div className="catalog-filters" aria-label="Catalog kind filters">
      {CATALOG_FILTERS.map((option) => (
        <Button
          type="button"
          color={filter === option ? "blue" : "gray"}
          highContrast={filter === option}
          size="1"
          variant={filter === option ? "solid" : "soft"}
          key={option}
          onClick={() => {
            onChange(option);
          }}
        >
          {filterLabel(option)}
        </Button>
      ))}
    </div>
  );
}

function CatalogContent({
  state,
  filter,
  busySkill,
  onChangeInstallation,
  onInstallAll,
  onError
}: Readonly<{
  state: AppState | null;
  filter: CatalogFilter;
  busySkill: string | null;
  onChangeInstallation: (item: CatalogItem) => Promise<void>;
  onInstallAll: (sourceId: string, bundleName: string | null) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  return (
    <>
      {state !== null && !state.ruleTarget.active && state.ruleTarget.message !== null && (
        <Callout.Root className="source-callout" color="amber" role="status" size="1" variant="surface">
          <Callout.Text>
            {state.ruleTarget.message} {state.ruleTarget.reloadRequired}
          </Callout.Text>
        </Callout.Root>
      )}
      {filter === "bundle" ? (
        <BundleList state={state} busySkill={busySkill} onChangeInstallation={onChangeInstallation} onInstallAll={onInstallAll} onError={onError} />
      ) : (
        <CatalogList state={state} busySkill={busySkill} filter={filter} onChangeInstallation={onChangeInstallation} onInstallAll={onInstallAll} onError={onError} />
      )}
    </>
  );
}

function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<ActionNotice | null>(null);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [sourceUrl, setSourceUrl] = useState("");
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [sourceMutationError, setSourceMutationError] = useState<string | null>(null);
  const [busySourceId, setBusySourceId] = useState<string | null>(null);
  const [addingSource, setAddingSource] = useState(false);
  const [filter, setFilter] = useState<CatalogFilter>("all");
  const lastCheckAttempt = useRef(0);
  const refreshPromise = useRef<Promise<void> | null>(null);
  const initialized = useRef(false);
  const mutationSequence = useRef(0);
  const lastMutationCompletedAtEpochSeconds = useRef(0);

  const runRefresh = useCallback(async (): Promise<void> => {
    lastCheckAttempt.current = Date.now();
    const mutationAtStart = mutationSequence.current;

    try {
      const payload = await invoke<unknown>("sync_app_state");
      const nextState = appStateSchema.parse(payload);
      if (mutationSequence.current === mutationAtStart) {
        startTransition(() => {
          setState(nextState);
        });
      }
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refresh = useCallback((): Promise<void> => {
    const inFlight = refreshPromise.current;
    if (inFlight !== null) {
      return inFlight;
    }

    setIsRefreshing(true);
    const promise = runRefresh().finally((): void => {
      if (refreshPromise.current === promise) {
        refreshPromise.current = null;
        setIsRefreshing(false);
      }
    });
    refreshPromise.current = promise;
    return promise;
  }, [runRefresh]);

  const initialize = useCallback(async (): Promise<void> => {
    const mutationAtStart = mutationSequence.current;
    try {
      const payload = await invoke<unknown>("load_cached_app_state");
      const cachedState = cachedAppStateSchema.parse(payload);
      if (cachedState !== null && mutationSequence.current === mutationAtStart) {
        startTransition(() => {
          setState(cachedState);
        });
      }
    } catch (reason) {
      setError(String(reason));
    }

    await refresh();
  }, [refresh]);

  useEffect(() => {
    let active = true;

    const refreshIfStale = (): void => {
      if (Date.now() - lastCheckAttempt.current < AUTO_UPDATE_INTERVAL_MS) {
        return;
      }

      refresh().catch((reason: unknown) => {
        setError(String(reason));
      });
    };

    if (!initialized.current) {
      initialized.current = true;
      initialize().catch((reason: unknown) => {
        setError(String(reason));
      });
    }

    const appWindow = getCurrentWindow();
    const scheduledSyncListener = appWindow.listen<unknown>(SCHEDULED_SYNC_EVENT, ({ payload }) => {
      if (!active) {
        return;
      }

      try {
        const result = scheduledSyncSchema.parse(payload);
        lastCheckAttempt.current = Date.now();

        if (result.kind === "failed") {
          setError(result.message);
          return;
        }
        if (result.state.checkedAtEpochSeconds <= lastMutationCompletedAtEpochSeconds.current) {
          return;
        }

        startTransition(() => {
          setState(result.state);
        });
        setError(null);
      } catch (reason) {
        setError(String(reason));
      }
    });
    scheduledSyncListener.catch((reason: unknown) => {
      if (active) {
        setError(String(reason));
      }
    });

    const focusListener = appWindow.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        refreshIfStale();
      }
    });
    focusListener.catch((reason: unknown) => {
      if (active) {
        setError(String(reason));
      }
    });

    return (): void => {
      active = false;
      scheduledSyncListener
        .then((unlisten) => {
          unlisten();
        })
        .catch(() => undefined);
      focusListener
        .then((unlisten) => {
          unlisten();
        })
        .catch(() => undefined);
    };
  }, [initialize, refresh]);

  async function changeInstallation(item: CatalogItem): Promise<void> {
    if (item.status === "modified" || item.status === "sourceConflict") {
      return;
    }
    if (item.status === "conflict") {
      const confirmed = await confirm(`Replace ${item.kind}:${item.name} with the copy from ${item.sourceName}? The existing managed surface will be backed up before replacement.`, {
        title: `Replace unmanaged ${item.kind}`,
        kind: "warning",
        okLabel: "Replace",
        cancelLabel: "Cancel"
      });
      if (!confirmed) {
        return;
      }
    }

    mutationSequence.current += 1;
    setBusySkill(itemIdentity(item));
    setError(null);
    setActionNotice(null);

    try {
      let nextNotice: ActionNotice | null = null;
      const sourceItem = { sourceId: item.sourceId, name: item.name };
      const commandSuffix = item.kind === "skill" ? "skill" : "rule";

      switch (item.status) {
        case "available":
        case "updateAvailable":
          await invoke<unknown>(`install_${commandSuffix}`, sourceItem);
          break;
        case "installed":
        case "removed":
          await invoke<unknown>(`uninstall_${commandSuffix}`, sourceItem);
          break;
        case "unmanagedMatch":
          await invoke<unknown>(`adopt_${commandSuffix}`, sourceItem);
          nextNotice = { kind: "adopted", sourceId: item.sourceId, sourceName: item.sourceName, name: item.name, itemKind: item.kind };
          break;
        case "conflict": {
          const payload = await invoke<unknown>(`replace_unmanaged_${commandSuffix}`, sourceItem);
          const replacement = replaceUnmanagedResultSchema.parse(payload);
          nextNotice = { kind: "replaced", sourceId: item.sourceId, sourceName: item.sourceName, name: item.name, itemKind: item.kind, backupPath: replacement.backupPath };
          break;
        }
      }

      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      startTransition(() => {
        setState((current) => (current === null ? null : stateAfterInstallationChange(current, item)));
      });
      setActionNotice(nextNotice);
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusySkill(null);
    }
  }

  async function installAll(sourceId: string, bundleName: string | null): Promise<void> {
    if (busySkill !== null) {
      return;
    }
    setBusySkill("bulk");
    setError(null);
    try {
      const payload = await invoke<unknown>("plan_install_all", { sourceId, bundleName });
      const plan = bulkPlanSchema.parse(payload);
      const lines = plan.entries.map((entry) => `${entry.action.padEnd(14)} ${entry.kind}:${entry.name}`).join("\n");
      if (plan.hasConflicts) {
        await confirm(`Nothing was changed. Resolve the attention items individually, then retry.\n\n${lines}`, {
          title: "Install plan needs attention",
          kind: "warning",
          okLabel: "Review items",
          cancelLabel: "Close"
        });
        setError("Bulk installation was not started because the plan contains manual adoption, replacement, modification, or source conflicts.");
        return;
      }
      const confirmed = await confirm(lines.length === 0 ? "This selection has no installable members." : `Apply this complete install plan?\n\n${lines}`, {
        title: bundleName === null ? "Install source items" : `Install ${bundleName}`,
        kind: "info",
        okLabel: "Install",
        cancelLabel: "Cancel"
      });
      if (!confirmed) {
        return;
      }
      mutationSequence.current += 1;
      const resultPayload = await invoke<unknown>("install_all", { sourceId, bundleName });
      const result = bulkInstallResultSchema.parse(resultPayload);
      if (result.failures.length > 0) {
        setError(`Some members failed after ${String(result.completed.length)} completed: ${result.failures.map((failure) => `${failure.kind}:${failure.name}: ${failure.message}`).join("; ")}`);
      }
      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusySkill(null);
    }
  }

  async function addSource(event: SyntheticEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const url = sourceUrl.trim();
    if (url.length === 0 || addingSource || busySourceId !== null) {
      return;
    }

    mutationSequence.current += 1;
    setAddingSource(true);
    setSourceError(null);
    setSourceMutationError(null);
    try {
      const payload = await invoke<unknown>("add_source", { url });
      const nextState = appStateSchema.parse(payload);
      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      startTransition(() => {
        setState(nextState);
      });
      setSourceUrl("");
      setError(null);
    } catch (reason) {
      setSourceError(String(reason));
    } finally {
      setAddingSource(false);
    }
  }

  async function removeSource(source: SourceState): Promise<void> {
    if (addingSource || busySourceId !== null) {
      return;
    }
    setBusySourceId(source.id);
    setSourceError(null);
    setSourceMutationError(null);
    try {
      const confirmed = await confirm(
        `Remove ${source.name} from Skill Manager? Its cached catalog will be deleted. Installed skills and rules from this source will remain available for safe uninstall.`,
        { title: "Remove source", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" }
      );
      if (!confirmed) {
        return;
      }

      mutationSequence.current += 1;
      const payload = await invoke<unknown>("remove_source", { sourceId: source.id });
      const nextState = appStateSchema.parse(payload);
      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      startTransition(() => {
        setState(nextState);
      });
      setError(null);
    } catch (reason) {
      setSourceMutationError(String(reason));
    } finally {
      setBusySourceId(null);
    }
  }

  async function addDefaultSource(): Promise<void> {
    if (addingSource || busySourceId !== null) {
      return;
    }
    mutationSequence.current += 1;
    setAddingSource(true);
    setSourceMutationError(null);
    try {
      const payload = await invoke<unknown>("add_default_source");
      const nextState = appStateSchema.parse(payload);
      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      startTransition(() => {
        setState(nextState);
      });
    } catch (reason) {
      setSourceMutationError(String(reason));
    } finally {
      setAddingSource(false);
    }
  }

  const summary = catalogSummary(state);
  const updateMessage = stateAutoUpdateMessage(state);
  const configuredSourceCount = sourceCount(state);

  return (
    <main className="app-shell">
      <motion.header className="hero" initial={{ opacity: 0, y: -10 }} animate={{ opacity: 1, y: 0 }} transition={ENTER_TRANSITION}>
        <Heading className="hero-title" as="h1" size="9" weight="bold">
          Skill Manager
        </Heading>
        <Text className="hero-copy" as="p" color="gray" size="3">
          Manage reusable skills, always-on Codex rules, and curated bundles on this computer. Closing this window keeps update checks running from the system tray.
        </Text>
      </motion.header>

      <NoticeStack
        error={error}
        updateMessage={updateMessage}
        actionNotice={actionNotice}
        onRetry={() => {
          refresh().catch((reason: unknown) => {
            setError(String(reason));
          });
        }}
        onDismissAction={() => {
          setActionNotice(null);
        }}
      />

      <motion.section className="catalog-stage" aria-labelledby="catalog-heading" initial={{ opacity: 0, y: 14 }} animate={{ opacity: 1, y: 0 }} transition={{ ...ENTER_TRANSITION, delay: 0.06 }}>
        <Card className="catalog" size="3" variant="surface">
          <div className="section-heading">
            <div>
              <Heading id="catalog-heading" as="h2" size="4" weight="bold">
                Catalog
              </Heading>
              <Text as="p" color="gray" size="2">
                {summary}
              </Text>
            </div>
            <div className="catalog-actions">
              <SourcesDialog
                open={sourcesOpen}
                sources={state?.sources ?? []}
                sourceUrl={sourceUrl}
                sourceError={sourceError}
                sourceMutationError={sourceMutationError}
                busySourceId={busySourceId}
                addingSource={addingSource}
                disabled={state === null || busySkill !== null || isRefreshing}
                onOpenChange={(open) => {
                  setSourcesOpen(open);
                  if (!open) {
                    setSourceError(null);
                    setSourceMutationError(null);
                  }
                }}
                onSourceUrlChange={(value) => {
                  setSourceUrl(value);
                  setSourceError(null);
                  setSourceMutationError(null);
                }}
                onAdd={(event) => {
                  addSource(event).catch((reason: unknown) => {
                    setSourceError(String(reason));
                  });
                }}
                onAddDefault={addDefaultSource}
                onRemove={removeSource}
                onError={setSourceMutationError}
              />
              <Button
                className="refresh-button"
                type="button"
                color="gray"
                highContrast
                loading={isRefreshing}
                size="2"
                variant="surface"
                onClick={() => {
                  refresh().catch((reason: unknown) => {
                    setError(String(reason));
                  });
                }}
                disabled={isRefreshing || busySkill !== null || addingSource || busySourceId !== null}
              >
                <RefreshIcon />
                {isRefreshing ? "Refreshing…" : "Refresh"}
              </Button>
            </div>
          </div>

          <CatalogFilters filter={filter} onChange={setFilter} />
          <CatalogContent
            state={state}
            filter={filter}
            busySkill={effectiveBusySkill(busySkill, addingSource, busySourceId)}
            onChangeInstallation={changeInstallation}
            onInstallAll={installAll}
            onError={setError}
          />
        </Card>
      </motion.section>

      <motion.footer initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ ...ENTER_TRANSITION, delay: 0.18 }}>
        <div>
          <Text as="span" color="gray" size="1">
            Sources
          </Text>
          <Code className="footer-code" color="gray" size="1" variant="ghost">
            {state === null ? "Loading…" : String(configuredSourceCount)}
          </Code>
        </div>
        <div>
          <Text as="span" color="gray" size="1">
            Skill location
          </Text>
          <Code className="footer-code" color="gray" size="1" variant="ghost">
            {state?.installRoot ?? "~/.agents/skills"}
          </Code>
        </div>
        <div>
          <Text as="span" color="gray" size="1">
            Codex rules
          </Text>
          <Code className="footer-code" color="gray" size="1" variant="ghost">
            {state?.ruleTarget.path ?? "~/.codex/AGENTS.md"}
          </Code>
        </div>
      </motion.footer>
    </main>
  );
}

export default App;
