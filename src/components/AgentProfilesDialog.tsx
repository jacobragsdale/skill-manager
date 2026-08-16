import type { JSX } from "react";
import { Badge, Button, Card, Dialog, Heading, Text } from "@radix-ui/themes";
import type { AgentProfile } from "../ipc/schemas";

export function AgentProfilesDialog({ open, profiles, onOpenChange }: Readonly<{ open: boolean; profiles: readonly AgentProfile[]; onOpenChange: (open: boolean) => void }>): JSX.Element {
  const detected = profiles.filter((profile) => profile.detected);
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="760px">
        <Dialog.Title>Detected agents</Dialog.Title>
        <Dialog.Description>
          Agent Plugins configures every coding agent it finds on this machine. Skills go to ~/.agents/skills, plus ~/.claude/skills when Claude Code is present. MCP servers are written to each
          detected agent.
        </Dialog.Description>
        <div className="agent-profiles">
          {profiles.map((profile) => (
            <Card key={profile.targetId} className="agent-profile">
              <div className="agent-profile-copy">
                <div className="skill-title-row">
                  <Heading as="h3" size="3">
                    {profile.displayName}
                  </Heading>
                  <Badge color={profile.detected ? "green" : "gray"} variant={profile.detected ? "solid" : "soft"}>
                    {profile.detected ? "Detected" : "Not detected"}
                  </Badge>
                </div>
                <Text as="p" color="gray" size="2">
                  {profile.detected
                    ? `Skills ${profile.skillDirectoryShared ? "shared at" : "at"} ${profile.skillDirectory}${profile.detectedVersion === null ? "" : ` · ${profile.detectedVersion}`}`
                    : "Not installed, so Agent Plugins will not write skills or MCP servers for it."}
                </Text>
                {profile.detectionMessage === null ? null : (
                  <Text as="p" color="amber" size="1">
                    {profile.detectionMessage}
                  </Text>
                )}
                {profile.detected ? (
                  <Text as="p" color="gray" size="1">
                    Verify: {profile.verificationGuidance} Reload: {profile.reloadGuidance}
                  </Text>
                ) : null}
              </div>
            </Card>
          ))}
        </div>
        {detected.length === 0 ? (
          <Text as="p" color="gray" size="2">
            Install Cursor, Claude Code, Codex, OpenCode, Grok Build, or GitHub Copilot, then refresh.
          </Text>
        ) : null}
        <div className="dialog-actions">
          <Dialog.Close>
            <Button variant="soft">Done</Button>
          </Dialog.Close>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}
