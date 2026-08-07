# Migrate to namespaced Agent Skill names

Manifest-aware sources change agent-visible skill names from a repository-local name such as `review` to a globally distinct name such as `skillbook-review`.

This guide explains the first-sync migration and how to resolve entries Skill Manager deliberately leaves alone.

## Before the first manifest-aware sync

1. Commit or copy any intentional local edits under `~/.agents/skills/`.
2. Ensure each configured source now publishes a valid `skill-manager.json` with its permanent `source.id`.
3. Check whether a future prefixed destination already exists, for example:

   ```bash
   test -e ~/.agents/skills/skillbook-review && echo "destination already exists"
   ```

4. Start Skill Manager and choose **Refresh**.

Migration is per item. A conflict in one skill does not prevent unrelated skills from migrating.

## What automatic migration does

For an unmodified, uniquely attributable legacy skill, Skill Manager:

1. reads the source-aware marker or proves an exact unique content match;
2. stages the source item at the new destination;
3. writes a materialized `SKILL.md` whose `name` is prefixed;
4. activates the new directory and records it in the installation ledger; and
5. removes the old directory only after activation succeeds.

For `review` in the default `skillbook` source:

```text
~/.agents/skills/review
        ↓
~/.agents/skills/skillbook-review
```

The catalog ID becomes `skillbook/review`, while ownership remains bound to the URL-derived source key. A different repository cannot take over an installation by publishing the same short namespace.

Exact unmanaged directories may migrate only when their source attribution is unique. An unmanaged symlink is moved to a backup first; Skill Manager never writes through it.

## Resolve a prefixed destination conflict

If `~/.agents/skills/skillbook-review` already contains different content, Skill Manager leaves both old and prefixed entries untouched and reports the conflict.

1. Compare the old, prefixed, and published content.
2. Preserve any content you need outside both managed destinations.
3. Remove or rename the conflicting prefixed path yourself.
4. Refresh to retry migration.

Do not merge local edits into the path and expect automatic migration to overwrite them. The digest protection is intentional.

## Resolve a locally modified managed skill

If the old managed directory no longer matches its recorded digest, Skill Manager marks it for manual attention and does not rename or delete it.

Choose one outcome:

- Preserve the edits as a separate personal skill with a new local name, then restore the managed copy to its published content and refresh.
- Move the edited directory outside `~/.agents/skills/`, then refresh and reinstall the namespaced item.
- Keep the old directory unmanaged and install the prefixed catalog item separately, provided their agent-visible names do not conflict in your agent runtime.

Skill Manager does not automatically decide which copy represents your intent.

## Resolve an ambiguous unmanaged match

When identical local names and content could belong to more than one source, attribution is not provably unique. Move the directory aside, install the desired canonical item explicitly, and then compare or remove the old copy.

The canonical item ID shown in the UI identifies the intended source: `source-id/local-id`.

## Verify the result

After migration:

1. Confirm the card shows the prefixed materialized name.
2. Expand **Details and destinations** and reveal each path.
3. Check that the installed directory name and `SKILL.md` frontmatter agree:

   ```bash
   skill_path="$HOME/.agents/skills/skillbook-review"
   test -d "$skill_path"
   sed -n '1,12p' "$skill_path/SKILL.md"
   ```

4. Confirm the old local-name directory is absent only for items that migrated successfully.
5. Run the relevant agent and invoke the new prefixed skill name.

## Failure and retry behavior

Activation failure restores the original legacy entry. Ledger writes and file activation are transactional for one item. A failed post-install hook is different: declarative files remain active and the item is marked **Incomplete** so retry runs the pending post-hook.

Refresh is retry-safe. Skill Manager rechecks ownership and current digests instead of assuming a previous migration step completed.
