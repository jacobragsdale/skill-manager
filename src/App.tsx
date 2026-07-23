import { useCallback, useEffect, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import "./App.css";

const skillStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict"]);

const skillSchema = z.strictObject({ name: z.string().min(1), description: z.string().min(1), status: skillStatusSchema }).readonly();

const appStateSchema = z
  .strictObject({
    installRoot: z.string().min(1),
    catalogSource: z.url(),
    catalogStatus: z.enum(["fresh", "cached"]),
    catalogMessage: z.string().min(1).nullable(),
    skills: z.array(skillSchema).readonly()
  })
  .readonly();

type SkillStatus = z.infer<typeof skillStatusSchema>;
type Skill = z.infer<typeof skillSchema>;
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

function App(): JSX.Element {
  const [state, setState] = useState<AppState | null>(null);
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async (): Promise<void> => {
    try {
      const payload = await invoke<unknown>("get_app_state");
      setState(appStateSchema.parse(payload));
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    refresh().catch((reason: unknown) => {
      setError(String(reason));
    });
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

  const catalogSkillCount = state === null ? 0 : state.skills.filter((skill) => skill.status !== "removed").length;
  const catalogSummary = state === null ? "Loading from GitHub…" : `${String(catalogSkillCount)} from skillbook${state.catalogStatus === "cached" ? " · cached" : ""}`;
  const catalogMessage = state?.catalogMessage ?? null;

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

      <section className="catalog" aria-labelledby="catalog-heading">
        <div className="section-heading">
          <div>
            <h2 id="catalog-heading">Skills</h2>
            <p>{catalogSummary}</p>
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
            Refresh
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
