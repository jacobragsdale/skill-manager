import type { JSX } from "react";
import { Button, Callout } from "@radix-ui/themes";

export function AgentSetupNotice({ visible, onChoose }: Readonly<{ visible: boolean; onChoose: () => void }>): JSX.Element | null {
  if (!visible) {
    return null;
  }
  return (
    <Callout.Root className="app-callout notice" color="blue" role="status">
      <div className="callout-content">
        <Callout.Text>
          No supported coding agent was detected. Portable skills and MCP servers cannot be installed until Cursor, Claude Code, Codex, OpenCode, Grok Build, or GitHub Copilot is on this machine.
        </Callout.Text>
        <Button className="callout-action" size="1" onClick={onChoose}>
          View Agents
        </Button>
      </div>
    </Callout.Root>
  );
}
