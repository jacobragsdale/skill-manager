import type { JSX } from "react";
import { Button, Callout } from "@radix-ui/themes";
import type { AppState } from "../ipc/schemas";
import { reportMessage } from "../lib/status";

export function Notice({ error, state, onDismiss }: Readonly<{ error: string | null; state: AppState | null; onDismiss: () => void }>): JSX.Element | null {
  const report = state === null ? null : reportMessage(state);
  if (error === null && report === null) {
    return null;
  }
  const isError = error !== null;
  return (
    <Callout.Root className="app-callout notice" color={isError ? "red" : "amber"} role={isError ? "alert" : "status"}>
      <div className="callout-content">
        <Callout.Text>{error ?? report}</Callout.Text>
        {isError ? (
          <Button className="callout-action" size="1" variant="soft" onClick={onDismiss}>
            Dismiss
          </Button>
        ) : null}
      </div>
    </Callout.Root>
  );
}
