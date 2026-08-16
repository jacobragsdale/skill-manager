import type { JSX } from "react";
import { Badge, Button, Card, Dialog, Heading, Text } from "@radix-ui/themes";
import { errorText } from "../ipc/client";
import type { AgentProfile, TargetId } from "../ipc/schemas";

export function AgentProfilesDialog({
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
            ? "Detected coding agents start enabled. Enable any others whose MCP servers and skill folders Agent Plugins should write."
            : "Detected agents start enabled. Disable an agent to stop Agent Plugins from writing its MCP config and skill bindings."}{" "}
          Skills use each agent's own folder when that keeps them off disabled agents. They share ~/.agents/skills when Codex is enabled or every agent that reads that folder is enabled — those agents
          can still discover shared-folder skills. Claude Code uses ~/.claude/skills; OpenCode may scan that folder too.
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
                <Text as="p" color="gray" size="2">
                  Skills {profile.skillDirectoryShared ? "shared at" : "at"} {profile.skillDirectory}
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
