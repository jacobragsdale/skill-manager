import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type SkillStatus = "available" | "installed" | "conflict";

type Skill = {
  name: string;
  description: string;
  status: SkillStatus;
};

type AppState = {
  installRoot: string;
  skills: Skill[];
};

function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setState(await invoke<AppState>("get_app_state"));
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function changeInstallation(skill: Skill) {
    setBusySkill(skill.name);
    setError(null);

    try {
      const command =
        skill.status === "installed" ? "uninstall_skill" : "install_skill";
      await invoke(command, { name: skill.name });
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusySkill(null);
    }
  }

  return (
    <main className="app-shell">
      <header className="hero">
        <div className="eyebrow">Local skill catalog</div>
        <h1>Skill Manager</h1>
        <p>Install small, reusable skills for every agent on this computer.</p>
      </header>

      {error && (
        <div className="error" role="alert">
          <span>{error}</span>
          <button className="text-button" type="button" onClick={refresh}>
            Try again
          </button>
        </div>
      )}

      <section className="catalog" aria-labelledby="catalog-heading">
        <div className="section-heading">
          <div>
            <h2 id="catalog-heading">Skills</h2>
            <p>{state ? `${state.skills.length} bundled` : "Loading catalog…"}</p>
          </div>
          <button
            className="secondary-button"
            type="button"
            onClick={refresh}
            disabled={!state || busySkill !== null}
          >
            Refresh
          </button>
        </div>

        <div className="skill-list">
          {state?.skills.map((skill) => {
            const busy = busySkill === skill.name;
            const installed = skill.status === "installed";
            const conflict = skill.status === "conflict";

            return (
              <article className="skill-card" key={skill.name}>
                <div className="skill-icon" aria-hidden="true">
                  {skill.name.slice(0, 1).toUpperCase()}
                </div>
                <div className="skill-copy">
                  <div className="skill-title-row">
                    <h3>{skill.name}</h3>
                    <span className={`status status-${skill.status}`}>
                      {conflict
                        ? "Already exists"
                        : installed
                          ? "Installed"
                          : "Available"}
                    </span>
                  </div>
                  <p>{skill.description}</p>
                </div>
                <button
                  className={installed ? "danger-button" : "primary-button"}
                  type="button"
                  disabled={busySkill !== null || conflict}
                  onClick={() => changeInstallation(skill)}
                >
                  {busy ? "Working…" : installed ? "Uninstall" : "Install"}
                </button>
              </article>
            );
          })}

          {!state && <div className="loading-card">Reading bundled skills…</div>}
        </div>
      </section>

      <footer>
        <span>Install location</span>
        <code>{state?.installRoot ?? "~/.agents/skills"}</code>
      </footer>
    </main>
  );
}

export default App;
