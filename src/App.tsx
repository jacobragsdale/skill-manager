import { startTransition, useCallback, useEffect, useRef, useState } from "react";
import type { JSX, ReactNode } from "react";
import { Badge, Button, Callout, Card, Code, Heading, Spinner, Text } from "@radix-ui/themes";
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

const skillStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "unmanagedMatch", "conflict"]);

const skillSchema = z.strictObject({ name: z.string().min(1), description: z.string().min(1), status: skillStatusSchema }).readonly();

const skillUpdateFailureSchema = z.strictObject({ name: z.string().min(1), message: z.string().min(1) }).readonly();

const replaceUnmanagedResultSchema = z.strictObject({ backupPath: z.string().min(1) }).readonly();

const autoUpdateReportSchema = z
  .strictObject({
    updatedSkills: z.array(z.string().min(1)).readonly(),
    skippedModifiedSkills: z.array(z.string().min(1)).readonly(),
    skippedLegacySkills: z.array(z.string().min(1)).readonly(),
    failedSkills: z.array(skillUpdateFailureSchema).readonly()
  })
  .readonly();

const appStateSchema = z
  .strictObject({
    installRoot: z.string().min(1),
    catalogSource: z.url(),
    catalogStatus: z.enum(["fresh", "cached"]),
    catalogMessage: z.string().min(1).nullable(),
    catalogCommit: z
      .string()
      .regex(/^[0-9a-f]{40}$/)
      .nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    autoUpdateReport: autoUpdateReportSchema,
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
type Skill = z.infer<typeof skillSchema>;
type AutoUpdateReport = z.infer<typeof autoUpdateReportSchema>;
type AppState = z.infer<typeof appStateSchema>;
type ActionNotice = Readonly<{ kind: "adopted"; name: string }> | Readonly<{ kind: "replaced"; name: string; backupPath: string }>;
type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

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
      return "amber";
    case "modified":
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
  }
}

function autoUpdateMessage(report: AutoUpdateReport): string | null {
  const messages: string[] = [];

  if (report.updatedSkills.length > 0) {
    messages.push(`Automatically updated ${report.updatedSkills.join(", ")}.`);
  }
  if (report.skippedModifiedSkills.length > 0) {
    messages.push(`Protected local changes in ${report.skippedModifiedSkills.join(", ")}.`);
  }
  if (report.skippedLegacySkills.length > 0) {
    messages.push(`Legacy installs require one manual update before automatic updates can manage them safely: ${report.skippedLegacySkills.join(", ")}.`);
  }
  if (report.failedSkills.length > 0) {
    messages.push(`Automatic update failed for ${report.failedSkills.map((failure) => `${failure.name}: ${failure.message}`).join("; ")}.`);
  }

  return messages.length === 0 ? null : messages.join(" ");
}

function catalogSummary(state: AppState | null): string {
  if (state === null) {
    return "Loading from GitHub…";
  }

  const skillCount = state.skills.filter((skill) => skill.status !== "removed").length;
  const commit = state.catalogCommit === null ? "" : ` · ${state.catalogCommit.slice(0, 7)}`;
  const cached = state.catalogStatus === "cached" ? " · cached" : "";
  const checkedAt = checkedAtFormatter.format(new Date(state.checkedAtEpochSeconds * 1000));
  return `${String(skillCount)} from skillbook${commit} · checked ${checkedAt}${cached}`;
}

function stateAutoUpdateMessage(state: AppState | null): string | null {
  return state === null ? null : autoUpdateMessage(state.autoUpdateReport);
}

function stateAfterInstallationChange(state: AppState, skill: Skill): AppState {
  if (skill.status === "removed") {
    return { ...state, skills: state.skills.filter((candidate) => candidate.name !== skill.name) };
  }

  const nextStatus = skill.status === "installed" ? "available" : "installed";
  return { ...state, skills: state.skills.map((candidate) => (candidate.name === skill.name ? { ...candidate, status: nextStatus } : candidate)) };
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

function ActionNoticeMessage({ notice }: Readonly<{ notice: ActionNotice }>): JSX.Element {
  return (
    <AppCallout color="green" role="status">
      {notice.kind === "adopted" ? (
        <span>{notice.name} is now managed by Skill Manager.</span>
      ) : (
        <span>
          Replaced {notice.name}. The original remains at <Code variant="ghost">{notice.backupPath}</Code>.
        </span>
      )}
    </AppCallout>
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

function SkillList({
  state,
  busySkill,
  onChangeInstallation,
  onError
}: Readonly<{ state: AppState | null; busySkill: string | null; onChangeInstallation: (skill: Skill) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  return (
    <div className="skill-list">
      <AnimatePresence initial mode="popLayout">
        {state?.skills.map((skill, index) => {
          const busy = busySkill === skill.name;
          const installed = skill.status === "installed";
          const removed = skill.status === "removed";
          const blocked = skill.status === "modified";
          const uninstall = installed || removed;
          const conflict = skill.status === "conflict";

          return (
            <motion.article
              className="skill-card-motion"
              key={skill.name}
              layout
              initial={{ opacity: 0, y: 10, scale: 0.99 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -8, scale: 0.99 }}
              transition={{ ...ENTER_TRANSITION, delay: Math.min(index * 0.035, 0.24) }}
            >
              <Card className="skill-card" size="2" variant="surface">
                <div className="skill-copy">
                  <div className="skill-title-row">
                    <Heading as="h3" size="3" weight="bold">
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
        })}

        {state === null && (
          <motion.div key="loading" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
            <Card className="loading-card" size="2" variant="surface">
              <Spinner size="2" />
              <div>
                <Text as="p" size="2" weight="medium">
                  Downloading skillbook catalog…
                </Text>
                <Text as="p" color="gray" size="1">
                  Checking GitHub for the latest skills.
                </Text>
              </div>
            </Card>
          </motion.div>
        )}

        {state !== null && state.skills.length === 0 && (
          <motion.div key="empty" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={QUICK_TRANSITION}>
            <Card className="loading-card" size="2" variant="surface">
              <Text as="p" color="gray" size="2">
                No skills are currently available.
              </Text>
            </Card>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<ActionNotice | null>(null);
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
    if (skill.status === "modified") {
      return;
    }
    if (skill.status === "conflict") {
      const confirmed = await confirm(`Replace ${skill.name}? Its current files will be moved to a backup outside the skills folder before the skillbook copy is installed.`, {
        title: "Replace unmanaged skill",
        kind: "warning",
        okLabel: "Replace",
        cancelLabel: "Cancel"
      });
      if (!confirmed) {
        return;
      }
    }

    mutationSequence.current += 1;
    setBusySkill(skill.name);
    setError(null);
    setActionNotice(null);

    try {
      let nextNotice: ActionNotice | null = null;

      switch (skill.status) {
        case "available":
        case "updateAvailable":
          await invoke<unknown>("install_skill", { name: skill.name });
          break;
        case "installed":
        case "removed":
          await invoke<unknown>("uninstall_skill", { name: skill.name });
          break;
        case "unmanagedMatch":
          await invoke<unknown>("adopt_skill", { name: skill.name });
          nextNotice = { kind: "adopted", name: skill.name };
          break;
        case "conflict": {
          const payload = await invoke<unknown>("replace_unmanaged_skill", { name: skill.name });
          const replacement = replaceUnmanagedResultSchema.parse(payload);
          nextNotice = { kind: "replaced", name: skill.name, backupPath: replacement.backupPath };
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

  const summary = catalogSummary(state);
  const catalogMessage = state?.catalogMessage ?? null;
  const updateMessage = stateAutoUpdateMessage(state);

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

      <div className="notice-stack">
        <AnimatePresence initial={false} mode="popLayout">
          {error !== null && (
            <AppCallout
              key="error"
              color="red"
              role="alert"
              action={
                <Button
                  className="callout-action"
                  type="button"
                  color="red"
                  size="1"
                  variant="ghost"
                  onClick={() => {
                    refresh().catch((reason: unknown) => {
                      setError(String(reason));
                    });
                  }}
                >
                  Try again
                </Button>
              }
            >
              {error}
            </AppCallout>
          )}

          {catalogMessage !== null && (
            <AppCallout key="catalog-message" color="amber" role="status">
              {catalogMessage}
            </AppCallout>
          )}

          {updateMessage !== null && (
            <AppCallout key="update-message" color="amber" role="status">
              {updateMessage}
            </AppCallout>
          )}

          {actionNotice !== null && <ActionNoticeMessage key={`${actionNotice.kind}-${actionNotice.name}`} notice={actionNotice} />}
        </AnimatePresence>
      </div>

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
              disabled={isRefreshing || busySkill !== null}
            >
              <RefreshIcon />
              {isRefreshing ? "Refreshing…" : "Refresh"}
            </Button>
          </div>

          <SkillList state={state} busySkill={busySkill} onChangeInstallation={changeInstallation} onError={setError} />
        </Card>
      </motion.section>

      <motion.footer initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ ...ENTER_TRANSITION, delay: 0.18 }}>
        <div>
          <Text as="span" color="gray" size="1">
            Source
          </Text>
          <Code className="footer-code" color="gray" size="1" variant="ghost">
            {state?.catalogSource ?? "github.com/jacobragsdale/skillbook"}
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
