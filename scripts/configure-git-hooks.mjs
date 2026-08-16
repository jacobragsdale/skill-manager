import { spawnSync } from "node:child_process";

const repository = spawnSync("git", ["rev-parse", "--is-inside-work-tree"], { stdio: "ignore" });

if (repository.status === 0) {
  const configured = spawnSync("git", ["config", "--local", "core.hooksPath", ".githooks"], { stdio: "inherit" });

  if (configured.error) {
    throw configured.error;
  }

  if (configured.status !== 0) {
    process.exit(configured.status ?? 1);
  }
}
