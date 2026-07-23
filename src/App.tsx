import { useCallback, useEffect, useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import "./App.css";

const AUTO_UPDATE_INTERVAL_MS = 15 * 60 * 1000;

const skillStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict"]);

const skillSchema = z.strictObject({ name: z.string().min(1), description: z.string().min(1), status: skillStatusSchema }).readonly();

const skillUpdateFailureSchema = z.strictObject({ name: z.string().min(1), message: z.string().min(1) }).readonly();

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

type SkillStatus = z.infer<typeof skillStatusSchema>;
type Skill = z.infer<typeof skillSchema>;
type AutoUpdateReport = z.infer<typeof autoUpdateReportSchema>;
type AppState = z.infer<typeof appStateSchema>;

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
    case "conflict":
      return "Unavailable";
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
  const checkedAt = new Date(state.checkedAtEpochSeconds * 1000).toLocaleTimeString();
  return `${String(skillCount)} from skillbook${commit} · checked ${checkedAt}${cached}`;
}

function stateAutoUpdateMessage(state: AppState | null): string | null {
  return state === null ? null : autoUpdateMessage(state.autoUpdateReport);
}

function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const lastCheckAttempt = useRef(0);

  const refresh = useCallback(async (): Promise<void> => {
    lastCheckAttempt.current = Date.now();

    try {
      const payload = await invoke<unknown>("sync_app_state");
      setState(appStateSchema.parse(payload));
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    const refreshIfStale = (): void => {
      if (Date.now() - lastCheckAttempt.current < AUTO_UPDATE_INTERVAL_MS) {
        return;
      }

      refresh().catch((reason: unknown) => {
        setError(String(reason));
      });
    };

    refresh().catch((reason: unknown) => {
      setError(String(reason));
    });

    const interval = window.setInterval(refreshIfStale, AUTO_UPDATE_INTERVAL_MS);
    window.addEventListener("focus", refreshIfStale);

    return (): void => {
      window.clearInterval(interval);
      window.removeEventListener("focus", refreshIfStale);
    };
  }, [refresh]);

  async function changeInstallation(skill: Skill): Promise<void> {
    setBusySkill(skill.name);
    setError(null);

    try {
      const command = skill.status === "installed" || skill.status === "removed" ? "uninstall_skill" : "install_skill";
      await invoke(command, { name: skill.name });
      await refresh();
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
            disabled={state === null || busySkill !== null}
          >
            Check now
          </button>
        </div>

        <div className="skill-list">
          {state?.skills.map((skill) => {
            const busy = busySkill === skill.name;
            const installed = skill.status === "installed";
            const removed = skill.status === "removed";
            const blocked = skill.status === "conflict" || skill.status === "modified";
            const uninstall = installed || removed;

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
                  className={uninstall ? "danger-button" : "primary-button"}
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
