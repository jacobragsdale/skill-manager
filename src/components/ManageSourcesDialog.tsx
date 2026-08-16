import type { JSX, ReactNode } from "react";
import { Badge, Button, Card, Dialog, Heading, Text } from "@radix-ui/themes";
import { errorText } from "../ipc/client";
import type { AppState, ListedSource, RepositoryState, SourceState } from "../ipc/schemas";

function ListedSourceCard({ name, description, children }: Readonly<{ name: string; description: string; children: ReactNode }>): JSX.Element {
  return (
    <Card className="listed-source-card">
      <div className="listed-source-copy">
        <Text as="p" size="2">
          {name}
        </Text>
        <Text as="p" color="gray" size="2">
          {description}
        </Text>
      </div>
      <div className="listed-source-actions">{children}</div>
    </Card>
  );
}

function listedSourceKey(listed: ListedSource): string {
  return listed.sourceId ?? listed.url;
}

function sourceForListed(state: AppState, listed: ListedSource): SourceState | null {
  return state.sources.find((source) => listed.sourceId !== null && source.sourceId === listed.sourceId) ?? state.sources.find((source) => source.url === listed.url) ?? null;
}

function orphanSources(state: AppState): readonly SourceState[] {
  const listedUrls = new Set(state.repositories.flatMap((repository) => repository.sources.map((listed) => listed.url)));
  const listedIds = new Set(state.repositories.flatMap((repository) => repository.sources.flatMap((listed) => (listed.sourceId === null ? [] : [listed.sourceId]))));
  return state.sources.filter((source) => !listedUrls.has(source.url) && !listedIds.has(source.sourceId));
}

export function ManageSourcesDialog({
  open,
  state,
  adding,
  removing,
  onOpenChange,
  onAddListed,
  onRemove,
  onError
}: Readonly<{
  open: boolean;
  state: AppState | null;
  adding: boolean;
  removing: ReadonlySet<string>;
  onOpenChange: (open: boolean) => void;
  onAddListed: (repository: RepositoryState, listed: ListedSource) => Promise<void>;
  onRemove: (source: SourceState) => Promise<void>;
  onError: (message: string) => void;
}>): JSX.Element {
  const repository = state?.repositories[0] ?? null;
  const extras = state === null ? [] : orphanSources(state);

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content maxWidth="720px">
        <Dialog.Title>Manage sources</Dialog.Title>
        <Dialog.Description>Adding a source makes its skills and MCP servers available. Nothing is installed until you choose it.</Dialog.Description>
        {state?.catalogMessage === null || state?.catalogMessage === undefined ? null : (
          <Text as="p" color="red" size="2">
            {state.catalogMessage}
          </Text>
        )}
        {state === null || repository === null ? (
          <Text as="p" color="gray" size="2">
            The source catalog is not configured yet. Once the company catalog URL is set, sources will appear here.
          </Text>
        ) : (
          <section className="manage-section">
            <div className="skill-title-row">
              <Heading as="h3" size="3">
                {repository.name}
              </Heading>
              {repository.refreshFailed ? <Badge color="red">Refresh failed</Badge> : null}
            </div>
            <Text as="p" color="gray" size="2">
              {repository.description}
            </Text>
            {repository.message === null ? null : (
              <Text as="p" color="red" size="2">
                {repository.message}
              </Text>
            )}
            {repository.sources.length === 0 ? (
              <Text as="p" color="gray" size="2">
                This catalog does not list any sources yet.
              </Text>
            ) : (
              <ul className="listed-sources">
                {repository.sources.map((listed) => {
                  const added = listed.alreadyAdded ? sourceForListed(state, listed) : null;
                  return (
                    <li key={listedSourceKey(listed)}>
                      <ListedSourceCard name={listed.name} description={listed.description}>
                        {listed.alreadyAdded ? (
                          added === null ? null : (
                            <Button
                              color="red"
                              size="1"
                              variant="soft"
                              loading={removing.has(added.sourceId)}
                              disabled={removing.has(added.sourceId)}
                              onClick={() => {
                                onRemove(added).catch((reason: unknown) => {
                                  onError(errorText(reason));
                                });
                              }}
                            >
                              Remove
                            </Button>
                          )
                        ) : (
                          <Button
                            size="1"
                            disabled={adding}
                            loading={adding}
                            onClick={() => {
                              onAddListed(repository, listed).catch((reason: unknown) => {
                                onError(errorText(reason));
                              });
                            }}
                          >
                            Add
                          </Button>
                        )}
                      </ListedSourceCard>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        )}
        {extras.length === 0 ? null : (
          <section className="manage-section">
            <Heading as="h3" size="3">
              Other sources
            </Heading>
            <Text as="p" color="gray" size="2">
              These sources are no longer listed in the catalog.
            </Text>
            <ul className="listed-sources">
              {extras.map((source) => (
                <li key={source.sourceKey}>
                  <ListedSourceCard name={source.name} description={source.description}>
                    <Button
                      color="red"
                      size="1"
                      variant="soft"
                      loading={removing.has(source.sourceId)}
                      disabled={removing.has(source.sourceId)}
                      onClick={() => {
                        onRemove(source).catch((reason: unknown) => {
                          onError(errorText(reason));
                        });
                      }}
                    >
                      Remove
                    </Button>
                  </ListedSourceCard>
                </li>
              ))}
            </ul>
          </section>
        )}
        <Text as="p" color="gray" size="2">
          Need a new source? Ask the catalog owner to add it.
        </Text>
        <div className="dialog-actions">
          <Dialog.Close>
            <Button variant="soft">Done</Button>
          </Dialog.Close>
        </div>
      </Dialog.Content>
    </Dialog.Root>
  );
}
