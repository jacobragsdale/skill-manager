import { startTransition, useCallback, useEffect, useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import { z } from "zod";
import "./App.css";

const AUTO_UPDATE_INTERVAL_MS = 15 * 60 * 1000;
const SCHEDULED_SYNC_EVENT = "scheduled-sync";

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
      return "Already exists";
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

function ActionNoticeMessage({ notice }: Readonly<{ notice: ActionNotice | null }>): JSX.Element | null {
  if (notice === null) {
    return null;
  }

  return (
    <div className="action-notice" role="status">
      {notice.kind === "adopted" ? (
        <span>{notice.name} is now managed by Skill Manager.</span>
      ) : (
        <span>
          Replaced {notice.name}. The original remains at <code>{notice.backupPath}</code>.
        </span>
      )}
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
      const confirmed = await confirm(
        `Replace ${skill.name}? Its current files will be moved to a backup outside the skills folder before the skillbook copy is installed. Automatic updates will never do this.`,
        { title: "Replace unmanaged skill", kind: "warning", okLabel: "Replace", cancelLabel: "Cancel" }
      );
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
      <header className="hero">
        <div className="eyebrow">GitHub-backed skill catalog</div>
        <h1>Skill Manager</h1>
        <p>Install small, reusable skills for every agent on this computer.</p>
      </header>

      {error !== null && (
        <div className="error" role="alert">
          <span>{error}</span>
          <button
            className="text-button"
            type="button"
            onClick={() => {
              refresh().catch((reason: unknown) => {
                setError(String(reason));
              });
            }}
          >
            Try again
          </button>
        </div>
      )}

      {catalogMessage !== null && (
        <div className="notice" role="status">
          {catalogMessage}
        </div>
      )}

      {updateMessage !== null && (
        <div className="notice" role="status">
          {updateMessage}
        </div>
      )}

      <ActionNoticeMessage notice={actionNotice} />

      <section className="catalog" aria-labelledby="catalog-heading">
        <div className="section-heading">
          <div>
            <h2 id="catalog-heading">Skills</h2>
            <p>{summary}</p>
          </div>
          <button
            className="secondary-button"
            type="button"
            onClick={() => {
              refresh().catch((reason: unknown) => {
                setError(String(reason));
              });
            }}
            disabled={isRefreshing || busySkill !== null}
          >
            {isRefreshing ? "Checking…" : "Check now"}
          </button>
        </div>

        <div className="skill-list">
          {state?.skills.map((skill) => {
            const busy = busySkill === skill.name;
            const installed = skill.status === "installed";
            const removed = skill.status === "removed";
            const blocked = skill.status === "modified";
            const uninstall = installed || removed;
            const destructive = uninstall || skill.status === "conflict";

            return (
              <article className="skill-card" key={skill.name}>
                <div className="skill-icon" aria-hidden="true">
                  {skill.name.slice(0, 1).toUpperCase()}
                </div>
                <div className="skill-copy">
                  <div className="skill-title-row">
                    <h3>{skill.name}</h3>
                    <span className={`status status-${skill.status}`}>{statusLabel(skill.status)}</span>
                  </div>
                  <p>{skill.description}</p>
                </div>
                <button
                  className={destructive ? "danger-button" : "primary-button"}
                  type="button"
                  disabled={busySkill !== null || blocked}
                  onClick={() => {
                    changeInstallation(skill).catch((reason: unknown) => {
                      setError(String(reason));
                    });
                  }}
                >
                  {actionLabel(skill.status, busy)}
                </button>
              </article>
            );
          })}

          {state === null && <div className="loading-card">Downloading skillbook catalog…</div>}
        </div>
      </section>

      <footer>
        <div>
          <span>Source</span>
          <code>{state?.catalogSource ?? "github.com/jacobragsdale/skillbook"}</code>
        </div>
        <div>
          <span>Install location</span>
          <code>{state?.installRoot ?? "~/.agents/skills"}</code>
        </div>
      </footer>
    </main>
  );
}

export default App;
