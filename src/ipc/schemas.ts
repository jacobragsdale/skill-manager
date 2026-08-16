import { z } from "zod";

export const itemStatusSchema = z.enum(["available", "installed", "updateAvailable", "removed", "modified", "conflict", "sourceConflict", "partiallyInstalled"]);
export const sourceStatusSchema = z.enum(["fresh", "cached", "error"]);
export const catalogErrorSchema = z.strictObject({ path: z.string().min(1), message: z.string().min(1) }).readonly();
export const targetIdSchema = z.enum(["cursor", "claude-code", "codex", "opencode", "grok-build", "github-copilot"]);
export const agentProfileSchema = z
  .strictObject({
    targetId: targetIdSchema,
    displayName: z.string().min(1),
    enabled: z.boolean(),
    scopes: z.array(z.string().min(1)).readonly(),
    dialectId: z.string().min(1),
    detected: z.boolean(),
    detectedVersion: z.string().min(1).nullable(),
    detectionMessage: z.string().min(1).nullable(),
    verificationGuidance: z.string().min(1),
    reloadGuidance: z.string().min(1)
  })
  .readonly();
export const componentSchema = z
  .strictObject({ id: z.string().min(1), kind: z.string().min(1), description: z.string().min(1), manualInvocation: z.boolean(), status: itemStatusSchema.optional() })
  .readonly();
export const capabilitySchema = z.discriminatedUnion("level", [
  z.strictObject({ level: z.literal("native") }).readonly(),
  z.strictObject({ level: z.literal("losslessTranslation") }).readonly(),
  z.strictObject({ level: z.literal("lossyTranslation"), losses: z.array(z.string().min(1)).readonly() }).readonly(),
  z.strictObject({ level: z.literal("unsupported"), reason: z.string().min(1) }).readonly(),
  z.strictObject({ level: z.literal("blocked"), reason: z.string().min(1), requiredAction: z.string().min(1) }).readonly()
]);
export const compatibilitySchema = z.strictObject({ componentId: z.string().min(1), targetId: z.string().min(1), capability: capabilitySchema }).readonly();
export const itemSchema = z
  .strictObject({
    id: z.string().min(3),
    localId: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    sourceName: z.string().min(1),
    sourceUrl: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    manualInvocation: z.boolean(),
    source: z.string().min(1),
    sourceIsDirectory: z.boolean(),
    manifestVersion: z.number().int().min(1).max(2),
    components: z.array(componentSchema).readonly(),
    compatibility: z.array(compatibilitySchema).readonly(),
    destination: z.string().min(1).nullable(),
    status: itemStatusSchema
  })
  .transform((item) => ({
    ...item,
    components: item.components.map((component) => ({
      id: component.id,
      kind: component.kind,
      description: component.description,
      manualInvocation: component.manualInvocation,
      status: component.status ?? item.status
    }))
  }))
  .readonly();
export const sourceSchema = z
  .strictObject({
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    repositoryKey: z.string().min(1).nullable().default(null),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    commit: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    catalogErrors: z.array(catalogErrorSchema).readonly()
  })
  .readonly();
export const listedSourceSchema = z
  .strictObject({ name: z.string().min(1), description: z.string().min(1), url: z.string().min(1), sourceId: z.string().min(2).nullable(), alreadyAdded: z.boolean() })
  .readonly();
export const repositorySchema = z
  .strictObject({
    repositoryId: z.string().min(2),
    repositoryKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    status: sourceStatusSchema,
    refreshFailed: z.boolean(),
    message: z.string().min(1).nullable(),
    revision: z.string().min(1).nullable(),
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    sources: z.array(listedSourceSchema).readonly()
  })
  .readonly();
export const itemReferenceSchema = z.strictObject({ id: z.string().min(1), sourceId: z.string().min(2), localId: z.string().min(1) }).readonly();
export const itemFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
export const autoUpdateReportSchema = z.strictObject({ updatedItems: z.array(itemReferenceSchema).readonly(), failedItems: z.array(itemFailureSchema).readonly() }).readonly();
export const appStateSchema = z
  .strictObject({
    checkedAtEpochSeconds: z.number().int().nonnegative(),
    autoUpdateReport: autoUpdateReportSchema,
    catalogMessage: z.string().min(1).nullable().default(null),
    repositories: z.array(repositorySchema).readonly().default([]),
    sources: z.array(sourceSchema).readonly(),
    items: z.array(itemSchema).readonly(),
    agentProfiles: z.array(agentProfileSchema).readonly()
  })
  .readonly();
export const preparedSourceSchema = z
  .strictObject({
    token: z.string().min(1),
    sourceId: z.string().min(2),
    sourceKey: z.string().min(1),
    name: z.string().min(1),
    description: z.string().min(1),
    url: z.string().min(1),
    commit: z.string().min(1),
    itemCount: z.number().int().nonnegative()
  })
  .readonly();
export const operationOutcomeSchema = z.strictObject({ backupPaths: z.array(z.string().min(1)).readonly() }).readonly();
export const bulkActionSchema = z.enum(["install", "replace", "uninstall"]);
export const bulkPlanEntrySchema = z.strictObject({ id: z.string().min(1), localId: z.string().min(1), status: itemStatusSchema, willRun: z.boolean() }).readonly();
export const bulkPlanSchema = z.strictObject({ sourceId: z.string().min(2), action: bulkActionSchema, entries: z.array(bulkPlanEntrySchema).readonly() }).readonly();
export const bulkFailureSchema = z.strictObject({ id: z.string().min(1), message: z.string().min(1) }).readonly();
export const bulkResultSchema = z
  .strictObject({ completed: z.array(z.string().min(1)).readonly(), failures: z.array(bulkFailureSchema).readonly(), backupPaths: z.array(z.string().min(1)).readonly() })
  .readonly();
export const removalPathSchema = z.strictObject({ path: z.string().min(1), modified: z.boolean() }).readonly();
export const removalItemSchema = z.strictObject({ id: z.string().min(1), paths: z.array(removalPathSchema).readonly() }).readonly();
export const sourceRemovalPlanSchema = z.strictObject({ sourceId: z.string().min(2), items: z.array(removalItemSchema).readonly() }).readonly();
export const targetCleanupPreviewSchema = z
  .strictObject({
    targetId: targetIdSchema,
    bindingCount: z.number().int().nonnegative(),
    resourcesRemoved: z.array(z.string().min(1)).readonly(),
    resourcesRetained: z.array(z.string().min(1)).readonly()
  })
  .readonly();
export const scheduledSyncSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("updated"), state: appStateSchema }).readonly(),
  z.strictObject({ kind: z.literal("failed"), message: z.string().min(1) }).readonly()
]);
export const cachedStateSchema = appStateSchema.nullable();
export const unitSchema = z.null();

export type AppState = z.infer<typeof appStateSchema>;
export type CatalogItem = z.infer<typeof itemSchema>;
export type CatalogComponent = CatalogItem["components"][number];
export type ItemStatus = z.infer<typeof itemStatusSchema>;
export type SourceState = z.infer<typeof sourceSchema>;
export type RepositoryState = z.infer<typeof repositorySchema>;
export type ListedSource = z.infer<typeof listedSourceSchema>;
export type BulkAction = z.infer<typeof bulkActionSchema>;
export type BulkPlan = z.infer<typeof bulkPlanSchema>;
export type AgentProfile = z.infer<typeof agentProfileSchema>;
export type TargetId = z.infer<typeof targetIdSchema>;
