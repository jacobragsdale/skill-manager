import type { JSX } from "react";
import { Badge, Button, Callout, Heading, Text } from "@radix-ui/themes";
import { openUrl } from "@tauri-apps/plugin-opener";
import { errorText } from "../ipc/client";
import type { BulkAction, CatalogItem, SourceState } from "../ipc/schemas";
import { repositoryBrowserUrl } from "../lib/repository-url";
import { supportsBulkAction } from "../lib/status";
import { ItemCard } from "./ItemCard";

export function SourceGroup({
  source,
  items,
  busyIds,
  allBusy,
  onItemChange,
  onBulk,
  onReset,
  onError
}: Readonly<{
  source: SourceState;
  items: readonly CatalogItem[];
  busyIds: ReadonlySet<string>;
  allBusy: boolean;
  onItemChange: (item: CatalogItem, componentId?: string) => Promise<void>;
  onBulk: (source: SourceState, action: BulkAction) => Promise<void>;
  onReset: (source: SourceState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const canInstall = items.some((item) => supportsBulkAction(item.status, "install"));
  const canReplace = items.some((item) => supportsBulkAction(item.status, "replace"));
  const canUninstall = items.some((item) => supportsBulkAction(item.status, "uninstall"));
  return (
    <section className="source-group">
      <div className="source-heading">
        <div>
          <div className="source-title-row">
            <Heading as="h3" size="4">
              <Button
                className="source-title-link"
                size="1"
                variant="ghost"
                onClick={() => {
                  openUrl(repositoryBrowserUrl(source.url) ?? source.url).catch((reason: unknown) => {
                    onError(errorText(reason));
                  });
                }}
              >
                {source.name}
              </Button>
            </Heading>
            {source.refreshFailed ? <Badge color="red">Refresh failed</Badge> : null}
          </div>
          <Text as="p" color="gray" size="2">
            {source.description}
          </Text>
        </div>
        <div className="source-group-actions">
          {canInstall ? (
            <Button
              size="1"
              variant="soft"
              color="green"
              disabled={allBusy}
              onClick={() => {
                onBulk(source, "install").catch((reason: unknown) => {
                  onError(errorText(reason));
                });
              }}
            >
              Install All
            </Button>
          ) : null}
          {canReplace ? (
            <Button
              size="1"
              variant="soft"
              color="amber"
              disabled={allBusy}
              onClick={() => {
                onBulk(source, "replace").catch((reason: unknown) => {
                  onError(errorText(reason));
                });
              }}
            >
              Replace All
            </Button>
          ) : null}
          {canUninstall ? (
            <Button
              size="1"
              variant="soft"
              color="red"
              disabled={allBusy}
              onClick={() => {
                onBulk(source, "uninstall").catch((reason: unknown) => {
                  onError(errorText(reason));
                });
              }}
            >
              Uninstall All
            </Button>
          ) : null}
          <Button
            size="1"
            variant="soft"
            color="red"
            disabled={allBusy}
            onClick={() => {
              onReset(source).catch((reason: unknown) => {
                onError(errorText(reason));
              });
            }}
          >
            Reset
          </Button>
        </div>
      </div>
      {source.message === null ? null : (
        <Callout.Root className="app-callout" color="red">
          <Callout.Text>{source.message}</Callout.Text>
        </Callout.Root>
      )}
      {source.catalogErrors.length === 0 ? null : (
        <Callout.Root className="app-callout" color="amber">
          <Callout.Text>{source.catalogErrors.map((catalogError) => `${catalogError.path}: ${catalogError.message}`).join(" · ")}</Callout.Text>
        </Callout.Root>
      )}
      <div className="skills-list">
        {items.map((item) => (
          <ItemCard key={item.id} item={item} sourceCommit={source.commit} busy={busyIds.has(item.id)} onChange={onItemChange} onError={onError} />
        ))}
        {items.length === 0 ? <Text color="gray">This source currently publishes no valid installs.</Text> : null}
      </div>
    </section>
  );
}
