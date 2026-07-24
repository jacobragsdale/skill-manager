import { startTransition, useCallback, useEffect, useRef, useState } from "react";
import type { JSX, ReactNode, SyntheticEvent } from "react";
import { Badge, Button, Callout, Card, Code, Dialog, Heading, Spinner, Text, TextField } from "@radix-ui/themes";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
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

const skillSchema = z
  .strictObject({ sourceId: z.string().min(1), sourceName: z.string().min(1), sourceUrl: z.string().min(1), name: z.string().min(1), description: z.string().min(1), status: skillStatusSchema })
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
    checkedAtEpochSeconds: z.number().int().nonnegative()
  })
  .readonly();

const autoUpdateSkillSchema = z.strictObject({ sourceId: z.string().min(1), name: z.string().min(1) }).readonly();
const skillUpdateFailureSchema = z.strictObject({ sourceId: z.string().min(1), name: z.string().min(1), message: z.string().min(1) }).readonly();
const replaceUnmanagedResultSchema = z.strictObject({ backupPath: z.string().min(1) }).readonly();

const autoUpdateReportSchema = z
  .strictObject({
    updatedSkills: z.array(autoUpdateSkillSchema).readonly(),
    skippedModifiedSkills: z.array(autoUpdateSkillSchema).readonly(),
    skippedLegacySkills: z.array(autoUpdateSkillSchema).readonly(),
    failedSkills: z.array(skillUpdateFailureSchema).readonly()
  })
  .readonly();

const appStateSchema = z
  .strictObject({
    installRoot: z.string().min(1),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    autoUpdateReport: autoUpdateReportSchema,
    sources: z.array(sourceStateSchema).readonly(),
    skills: z.array(skillSchema).readonly()
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
type Skill = z.infer<typeof skillSchema>;
type SourceState = z.infer<typeof sourceStateSchema>;
type AutoUpdateSkill = z.infer<typeof autoUpdateSkillSchema>;
type AutoUpdateReport = z.infer<typeof autoUpdateReportSchema>;
type AppState = z.infer<typeof appStateSchema>;
type SkillGroup = Readonly<{ id: string; name: string; url: string; source: SourceState | null; skills: readonly Skill[] }>;
type ActionNotice =
  Readonly<{ kind: "adopted"; sourceId: string; sourceName: string; name: string }> | Readonly<{ kind: "replaced"; sourceId: string; sourceName: string; name: string; backupPath: string }>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

function skillIdentity(skill: Skill): string {
  return `${skill.sourceId}\u0000${skill.name}`;
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
    messages.push(`Automatically updated ${report.updatedSkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`);
  }
  if (report.skippedModifiedSkills.length > 0) {
    messages.push(`Protected local changes in ${report.skippedModifiedSkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`);
  }
  if (report.skippedLegacySkills.length > 0) {
    messages.push(
      `Legacy installs require one manual update before automatic updates can manage them safely: ${report.skippedLegacySkills.map((skill) => reportSkillLabel(skill, sources)).join(", ")}.`
    );
  }
  if (report.failedSkills.length > 0) {
    messages.push(`Automatic update failed for ${report.failedSkills.map((failure) => `${reportSkillLabel(failure, sources)}: ${failure.message}`).join("; ")}.`);
  }

  return messages.length === 0 ? null : messages.join(" ");
}

function catalogSummary(state: AppState | null): string {
  if (state === null) {
    return "Loading skill sources…";
  }

  const skillCount = state.skills.filter((skill) => skill.status !== "removed").length;
  const sourceCount = state.sources.length;
  const cachedCount = state.sources.filter((source) => source.status === "cached").length;
  const errorCount = state.sources.filter((source) => source.status === "error").length;
  const checkedAt = checkedAtFormatter.format(new Date(state.checkedAtEpochSeconds * 1000));
  const cached = cachedCount === 0 ? "" : ` · ${String(cachedCount)} cached`;
  const errors = errorCount === 0 ? "" : ` · ${String(errorCount)} failed`;
  return `${String(skillCount)} skill${skillCount === 1 ? "" : "s"} from ${String(sourceCount)} source${sourceCount === 1 ? "" : "s"} · checked ${checkedAt}${cached}${errors}`;
}

function sourceCheckedAt(source: SourceState): string {
  if (source.checkedAtEpochSeconds === 0) {
    return "Not checked yet";
  }
  return `Checked ${checkedAtFormatter.format(new Date(source.checkedAtEpochSeconds * 1000))}`;
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

function sourceGroups(state: AppState): readonly SkillGroup[] {
  const activeGroups = state.sources.map((source): SkillGroup => {
    return { id: source.id, name: source.name, url: source.url, source, skills: state.skills.filter((skill) => skill.sourceId === source.id) };
  });
  const knownIds = new Set(state.sources.map((source) => source.id));
  const orphanSkills = state.skills.filter((skill) => !knownIds.has(skill.sourceId));
  const orphanGroups = orphanSkills
    .filter((skill, index, skills) => skills.findIndex((candidate) => candidate.sourceId === skill.sourceId) === index)
    .map((skill): SkillGroup => {
      return { id: skill.sourceId, name: skill.sourceName, url: skill.sourceUrl, source: null, skills: orphanSkills.filter((candidate) => candidate.sourceId === skill.sourceId) };
    });
  return [...activeGroups, ...orphanGroups];
}

function stateAfterInstallationChange(state: AppState, selectedSkill: Skill): AppState {
  const uninstalling = selectedSkill.status === "installed" || selectedSkill.status === "removed";

  if (selectedSkill.status === "removed") {
    return {
      ...state,
      skills: state.skills
        .filter((skill) => skillIdentity(skill) !== skillIdentity(selectedSkill))
        .map((skill) => (skill.name === selectedSkill.name && skill.status === "sourceConflict" ? { ...skill, status: "available" } : skill))
    };
  }

  return {
    ...state,
    skills: state.skills.map((skill) => {
      if (skillIdentity(skill) === skillIdentity(selectedSkill)) {
        return { ...skill, status: uninstalling ? "available" : "installed" };
      }
      if (skill.name === selectedSkill.name && skill.status === "sourceConflict" && uninstalling) {
        return { ...skill, status: "available" };
      }
      if (skill.name === selectedSkill.name && !uninstalling) {
        return { ...skill, status: "sourceConflict" };
      }
      return skill;
    })
  };
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
          {notice.name} from {notice.sourceName} is now managed by Skill Manager.
        </span>
      ) : (
        <span>
          Replaced {notice.name} from {notice.sourceName}. The original remains at <Code variant="ghost">{notice.backupPath}</Code>.
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
        <Callout.Text>This source was removed. Installed skills remain available for safe uninstall.</Callout.Text>
      </Callout.Root>
    );
  }
  if (source.message === null) {
    return null;
  }
  return (
    <Callout.Root className="source-callout" color={source.status === "error" ? "red" : "amber"} role={source.status === "error" ? "alert" : "status"} size="1" variant="surface">
      <Callout.Text>{source.message}</Callout.Text>
    </Callout.Root>
  );
}

function SkillCard({
  skill,
  busySkill,
  index,
  onChangeInstallation,
  onError
}: Readonly<{ skill: Skill; busySkill: string | null; index: number; onChangeInstallation: (skill: Skill) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const identity = skillIdentity(skill);
  const busy = busySkill === identity;
  const installed = skill.status === "installed";
  const removed = skill.status === "removed";
  const blocked = skill.status === "modified" || skill.status === "sourceConflict";
  const uninstall = installed || removed;
  const conflict = skill.status === "conflict";

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
              {skill.name}
            </Heading>
            <AnimatePresence initial={false} mode="wait">
              <motion.span
                className="status-motion"
                key={skill.status}
                initial={{ opacity: 0, scale: 0.94 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.94 }}
                transition={QUICK_TRANSITION}
              >
                <Badge color={statusColor(skill.status)} highContrast radius="full" size="1" variant="soft">
                  {statusLabel(skill.status)}
                </Badge>
              </motion.span>
            </AnimatePresence>
          </div>
          <Text as="p" color="gray" size="2">
            {skill.description}
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
            onChangeInstallation(skill).catch((reason: unknown) => {
              onError(String(reason));
            });
          }}
        >
          {actionLabel(skill.status, busy)}
        </Button>
      </Card>
    </motion.article>
  );
}

function SkillGroupSection({
  group,
  busySkill,
  startIndex,
  onChangeInstallation,
  onError
}: Readonly<{ group: SkillGroup; busySkill: string | null; startIndex: number; onChangeInstallation: (skill: Skill) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
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
          <Code className="source-url" color="gray" size="1" variant="ghost">
            {group.url}
          </Code>
        </div>
        <Text as="span" color="gray" size="1">
          {String(group.skills.length)} skill{group.skills.length === 1 ? "" : "s"}
        </Text>
      </div>

      <SourceMessage source={group.source} />

      <div className="source-skill-list">
        {group.skills.map((skill, index) => (
          <SkillCard key={skillIdentity(skill)} skill={skill} busySkill={busySkill} index={startIndex + index} onChangeInstallation={onChangeInstallation} onError={onError} />
        ))}
        {group.skills.length === 0 && (
          <Card className="empty-source-card" size="2" variant="surface">
            <Text as="p" color="gray" size="2">
              No skills found in this source.
            </Text>
          </Card>
        )}
      </div>
    </section>
  );
}

function SkillList({
  state,
  busySkill,
  onChangeInstallation,
  onError
}: Readonly<{ state: AppState | null; busySkill: string | null; onChangeInstallation: (skill: Skill) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  if (state === null) {
    return (
      <div className="skill-list">
        <Card className="loading-card" size="2" variant="surface">
          <Spinner size="2" />
          <div>
            <Text as="p" size="2" weight="medium">
              Loading skill sources…
            </Text>
            <Text as="p" color="gray" size="1">
              Checking each repository for the latest skills.
            </Text>
          </div>
        </Card>
      </div>
    );
  }

  const groups = sourceGroups(state);
  let startIndex = 0;

  return (
    <div className="skill-list">
      <AnimatePresence initial mode="popLayout">
        {groups.map((group) => {
          const groupStartIndex = startIndex;
          startIndex += group.skills.length;
          return (
            <motion.div className="source-group-motion" key={group.id} layout exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
              <SkillGroupSection group={group} busySkill={busySkill} startIndex={groupStartIndex} onChangeInstallation={onChangeInstallation} onError={onError} />
            </motion.div>
          );
        })}

        {groups.length === 0 && (
          <motion.div key="empty" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
            <Card className="loading-card" size="2" variant="surface">
              <Text as="p" color="gray" size="2">
                No skill sources are configured.
              </Text>
            </Card>
          </motion.div>
        )}
      </AnimatePresence>
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
              Built in
            </Badge>
          )}
          <Badge color={sourceStatusColor(source.status)} highContrast radius="full" size="1" variant="soft">
            {sourceStatusLabel(source.status)}
          </Badge>
        </div>
        {!source.builtIn && (
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
        )}
      </div>
      <Code className="source-item-url" color="gray" size="1" variant="ghost">
        {source.url}
      </Code>
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
          Add Git repositories that use the <Code variant="ghost">skills/&lt;name&gt;/SKILL.md</Code> layout.
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

  async function changeInstallation(skill: Skill): Promise<void> {
    if (skill.status === "modified" || skill.status === "sourceConflict") {
      return;
    }
    if (skill.status === "conflict") {
      const confirmed = await confirm(
        `Replace ${skill.name} with the copy from ${skill.sourceName}? Its current files will be moved to a backup outside the skills folder before the new copy is installed.`,
        { title: "Replace unmanaged skill", kind: "warning", okLabel: "Replace", cancelLabel: "Cancel" }
      );
      if (!confirmed) {
        return;
      }
    }

    mutationSequence.current += 1;
    setBusySkill(skillIdentity(skill));
    setError(null);
    setActionNotice(null);

    try {
      let nextNotice: ActionNotice | null = null;
      const sourceSkill = { sourceId: skill.sourceId, name: skill.name };

      switch (skill.status) {
        case "available":
        case "updateAvailable":
          await invoke<unknown>("install_skill", sourceSkill);
          break;
        case "installed":
        case "removed":
          await invoke<unknown>("uninstall_skill", sourceSkill);
          break;
        case "unmanagedMatch":
          await invoke<unknown>("adopt_skill", sourceSkill);
          nextNotice = { kind: "adopted", sourceId: skill.sourceId, sourceName: skill.sourceName, name: skill.name };
          break;
        case "conflict": {
          const payload = await invoke<unknown>("replace_unmanaged_skill", sourceSkill);
          const replacement = replaceUnmanagedResultSchema.parse(payload);
          nextNotice = { kind: "replaced", sourceId: skill.sourceId, sourceName: skill.sourceName, name: skill.name, backupPath: replacement.backupPath };
          break;
        }
      }

      lastMutationCompletedAtEpochSeconds.current = Math.floor(Date.now() / 1000);
      startTransition(() => {
        setState((current) => (current === null ? null : stateAfterInstallationChange(current, skill)));
      });
      setActionNotice(nextNotice);
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
    if (source.builtIn || addingSource || busySourceId !== null) {
      return;
    }
    setBusySourceId(source.id);
    setSourceError(null);
    setSourceMutationError(null);
    try {
      const confirmed = await confirm(`Remove ${source.name} from Skill Manager? Its cached catalog will be deleted. Any installed skills from this source will remain available for safe uninstall.`, {
        title: "Remove skill source",
        kind: "warning",
        okLabel: "Remove",
        cancelLabel: "Cancel"
      });
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
          Install small, reusable skills for every agent on this computer.
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
                Skills
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

          <SkillList state={state} busySkill={effectiveBusySkill(busySkill, addingSource, busySourceId)} onChangeInstallation={changeInstallation} onError={setError} />
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
            Install location
          </Text>
          <Code className="footer-code" color="gray" size="1" variant="ghost">
            {state?.installRoot ?? "~/.agents/skills"}
          </Code>
        </div>
      </motion.footer>
    </main>
  );
}

export default App;
