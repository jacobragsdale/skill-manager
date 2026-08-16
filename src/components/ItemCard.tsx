import type { JSX } from "react";
import { Badge, Button, Card, Heading, Text } from "@radix-ui/themes";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { errorText } from "../ipc/client";
import type { CatalogComponent, CatalogItem } from "../ipc/schemas";
import { repositoryPathBrowserUrl } from "../lib/repository-url";
import { componentLabel, primaryActionColor, primaryActionLabel, statusColor, statusLabel } from "../lib/status";

export function ItemCard({
  item,
  sourceCommit,
  busy,
  onChange,
  onError
}: Readonly<{ item: CatalogItem; sourceCommit: string | null; busy: boolean; onChange: (item: CatalogItem, componentId?: string) => Promise<void>; onError: (message: string) => void }>): JSX.Element {
  const protectedItem = item.status === "modified" || item.status === "sourceConflict";
  const sourceBrowserUrl = sourceCommit === null || item.status === "removed" ? null : repositoryPathBrowserUrl(item.sourceUrl, sourceCommit, item.source, item.sourceIsDirectory);
  const destination = item.destination;
  const expandable = item.components.length > 1;
  return (
    <Card className="skill-card">
      <div className="skill-card-main">
        <div className="skill-copy">
          <div className="skill-title-row">
            <Heading as="h4" size="3">
              {item.name}
            </Heading>
            {item.components.map((component) => (
              <Badge key={`${component.kind}:${component.id}`} color="gray" variant="soft">
                {componentLabel(component.kind)}
              </Badge>
            ))}
            {item.status === "available" || item.status === "installed" ? null : <Badge color={statusColor(item.status)}>{statusLabel(item.status)}</Badge>}
            {item.manualInvocation ? <Badge color="blue">Manual Invocation</Badge> : null}
          </div>
          <Text as="p" color="gray" size="2">
            {item.description}
          </Text>
          <details className="item-details">
            <summary>Source and managed resource</summary>
            <dl>
              <dt>Source</dt>
              <dd>
                {sourceBrowserUrl === null ? (
                  item.source
                ) : (
                  <Button
                    className="source-path-link"
                    size="1"
                    variant="ghost"
                    onClick={() => {
                      openUrl(sourceBrowserUrl).catch((reason: unknown) => {
                        onError(errorText(reason));
                      });
                    }}
                  >
                    {item.source}
                  </Button>
                )}
              </dd>
              {destination === null ? null : (
                <>
                  <dt>Primary resource</dt>
                  <dd>
                    <Button
                      className="destination-link"
                      size="1"
                      variant="ghost"
                      onClick={() => {
                        revealItemInDir(destination).catch((reason: unknown) => {
                          onError(errorText(reason));
                        });
                      }}
                    >
                      {destination}
                    </Button>
                  </dd>
                </>
              )}
            </dl>
          </details>
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
        <details className="component-list">
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
        </div>
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
