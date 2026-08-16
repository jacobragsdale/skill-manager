import { useState, type JSX } from "react";
import { Badge, Button, Card, Heading, Text } from "@radix-ui/themes";
import { errorText } from "../ipc/client";
import type { CatalogComponent, CatalogItem } from "../ipc/schemas";
import { componentLabel, primaryActionColor, primaryActionLabel, statusColor, statusLabel } from "../lib/status";

export function ItemCard({
  item,
  busy,
  onChange,
  onError
}: Readonly<{ item: CatalogItem; busy: boolean; onChange: (item: CatalogItem, componentId?: string) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const protectedItem = item.status === "modified" || item.status === "sourceConflict";
  const expandable = item.components.length > 1;
  const [componentsOpen, setComponentsOpen] = useState(true);
  return (
    <Card className="skill-card">
      <div className="skill-card-main">
        <div className="skill-copy">
          <div className="skill-title-row">
            <Heading as="h4" size="3">
              {item.name}
            </Heading>
            {item.status === "available" || item.status === "installed" ? null : <Badge color={statusColor(item.status)}>{statusLabel(item.status)}</Badge>}
            {item.manualInvocation ? <Badge color="blue">Manual Invocation</Badge> : null}
          </div>
          <Text as="p" color="gray" size="2">
            {item.description}
          </Text>
        </div>
        <div className="item-actions">
          <Button
            className="skill-action skill-action-primary"
            color={primaryActionColor(item.status)}
            disabled={busy || protectedItem}
            loading={busy}
            onClick={() => {
              onChange(item).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            {packageActionLabel(item)}
          </Button>
        </div>
      </div>
      {expandable ? (
        <details
          className="component-list"
          open={componentsOpen}
          onToggle={(event) => {
            setComponentsOpen(event.currentTarget.open);
          }}
        >
          <summary>
            {String(item.components.length)} items · {componentSummary(item.components)}
          </summary>
          <ul>
            {item.components.map((component) => (
              <li key={`${component.kind}:${component.id}`}>
                <ComponentRow component={component} busy={busy} protectedItem={protectedItem} onChange={() => onChange(item, component.id)} onError={onError} />
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </Card>
  );
}

function packageActionLabel(item: CatalogItem): string {
  if (item.status === "partiallyInstalled" && item.components.some((component) => component.status === "available")) {
    return "Install remaining";
  }
  return primaryActionLabel(item.status);
}

function componentSummary(components: readonly CatalogComponent[]): string {
  const skills = components.filter((component) => component.kind === "skill").length;
  const servers = components.filter((component) => component.kind === "mcpServer").length;
  const parts: string[] = [];
  if (skills > 0) {
    parts.push(`${String(skills)} skill${skills === 1 ? "" : "s"}`);
  }
  if (servers > 0) {
    parts.push(`${String(servers)} MCP`);
  }
  return parts.join(" · ");
}

function ComponentRow({
  component,
  busy,
  protectedItem,
  onChange,
  onError
}: Readonly<{ component: CatalogComponent; busy: boolean; protectedItem: boolean; onChange: () => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const blocked = protectedItem || component.status === "modified" || component.status === "sourceConflict";
  return (
    <div className="component-row">
      <div className="component-copy">
        <div className="skill-title-row">
          <Badge color="gray" variant="soft">
            {componentLabel(component.kind)}
          </Badge>
          <Text size="2">{component.id}</Text>
          {component.status === "available" || component.status === "installed" ? null : <Badge color={statusColor(component.status)}>{statusLabel(component.status)}</Badge>}
          {component.manualInvocation ? <Badge color="blue">Manual Invocation</Badge> : null}
        </div>
        <Text as="p" color="gray" size="2">
          {component.description}
        </Text>
      </div>
      <Button
        className="skill-action"
        size="1"
        color={primaryActionColor(component.status)}
        disabled={busy || blocked}
        loading={busy}
        onClick={() => {
          onChange().catch((reason: unknown) => {
            onError(errorText(reason));
          });
        }}
      >
        {primaryActionLabel(component.status)}
      </Button>
    </div>
  );
}
