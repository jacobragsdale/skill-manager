import { confirm } from "@tauri-apps/plugin-dialog";
import { invokeParsed } from "../ipc/client";
import { targetCleanupPreviewSchema } from "../ipc/schemas";
import type { AgentProfile, AppState, BulkAction, BulkPlan, CatalogComponent, CatalogItem, ItemStatus, SourceState } from "../ipc/schemas";

export type AccentColor = "amber" | "blue" | "gray" | "green" | "red";

export function statusLabel(status: ItemStatus): string {
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

export function statusColor(status: ItemStatus): AccentColor {
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

export function primaryActionLabel(status: ItemStatus): string {
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

export function primaryActionColor(status: ItemStatus): AccentColor {
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

export function componentLabel(kind: string): string {
  switch (kind) {
    case "skill":
      return "Skill";
    case "mcpServer":
      return "MCP";
    default:
      return kind;
  }
}

export function itemCommandArgs(item: CatalogItem, componentId: string | undefined, extra: Record<string, unknown>): Record<string, unknown> {
  return componentId === undefined ? { sourceId: item.sourceId, localId: item.localId, ...extra } : { sourceId: item.sourceId, localId: item.localId, componentId, ...extra };
}

export function uninstallMessage(item: CatalogItem, component: CatalogComponent | undefined): string {
  if (component === undefined) {
    return `Remove every unshared managed resource for ${item.name}? Shared resources still used by another agent will remain.`;
  }
  return `Remove ${componentLabel(component.kind)} ${component.id} from ${item.name}? Other items in this package stay installed.`;
}

export function commandForStatus(status: ItemStatus): "install_item" | "replace_item" | "uninstall_item" | null {
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

export async function reviewReplace(): Promise<boolean> {
  return confirm("Existing unmanaged files will be backed up, then replaced.", { title: "Replace", kind: "warning", okLabel: "Back Up and Replace", cancelLabel: "Cancel" });
}

export function bulkLabels(action: BulkAction): Readonly<{ action: string; title: string; button: string; warning: string }> {
  switch (action) {
    case "install":
      return { action: "Install or update", title: "Install all", button: "Install", warning: "" };
    case "replace":
      return { action: "Replace", title: "Replace all", button: "Replace", warning: " Existing destinations will be backed up before replacement." };
    case "uninstall":
      return { action: "Uninstall", title: "Uninstall all", button: "Uninstall", warning: "" };
  }
}

export async function reviewBulk(source: SourceState, action: BulkAction, plan: BulkPlan): Promise<boolean> {
  const eligible = plan.entries.filter((entry) => entry.willRun);
  const labels = bulkLabels(action);
  return confirm(`${labels.action} ${String(eligible.length)} item${eligible.length === 1 ? "" : "s"} from ${source.name}?${labels.warning}`, {
    title: labels.title,
    kind: action === "install" ? "info" : "warning",
    okLabel: labels.button,
    cancelLabel: "Cancel"
  });
}

export async function reviewAgentEnable(profile: AgentProfile): Promise<boolean> {
  return confirm(`${profile.verificationGuidance} ${profile.reloadGuidance}`, { title: `Enable ${profile.displayName}`, kind: "info", okLabel: "Enable", cancelLabel: "Cancel" });
}

export async function reviewAgentDisable(profile: AgentProfile): Promise<boolean> {
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

export function supportsBulkAction(status: ItemStatus, action: BulkAction): boolean {
  switch (action) {
    case "install":
      return status === "available" || status === "updateAvailable" || status === "partiallyInstalled";
    case "replace":
      return status === "conflict";
    case "uninstall":
      return status === "installed" || status === "updateAvailable" || status === "partiallyInstalled";
  }
}

export function hasEnabledAgent(profiles: readonly AgentProfile[]): boolean {
  return profiles.some((profile) => profile.enabled);
}

export function reportMessage(state: AppState): string | null {
  const parts: string[] = [];
  if (state.autoUpdateReport.updatedItems.length > 0) {
    parts.push(`Updated ${state.autoUpdateReport.updatedItems.map((item) => item.id).join(", ")}.`);
  }
  if (state.autoUpdateReport.failedItems.length > 0) {
    parts.push(`Background updates failed: ${state.autoUpdateReport.failedItems.map((item) => `${item.id}: ${item.message}`).join("; ")}.`);
  }
  return parts.length === 0 ? null : parts.join(" ");
}
