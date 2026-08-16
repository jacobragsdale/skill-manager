import type { JSX } from "react";
import { Button, Callout } from "@radix-ui/themes";

export function AgentSetupNotice({ visible, onChoose }: Readonly<{ visible: boolean; onChoose: () => void }>): JSX.Element | null {
  if (!visible) {
    return null;
  }
  return (
    <Callout.Root className="app-callout notice" color="blue" role="status">
      <div className="callout-content">
        <Callout.Text>Select the agents you use. Portable skills and MCP servers cannot be installed or updated until at least one agent is enabled.</Callout.Text>
        <Button className="callout-action" size="1" onClick={onChoose}>
          Choose Agents
        </Button>
      </div>
    </Callout.Root>
  );
}
