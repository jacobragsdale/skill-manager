//! Ledger and filesystem matching for planned resources.

use super::{path_entry_exists, validate_absolute_owned_path};
use crate::ledger::{self, InstallationLedger, OwnedResource, ResourceRecord};
use crate::managed_documents;
use crate::paths::SystemPaths;
use crate::resource::OperationPlan;
use std::fs;
use std::path::Path;

pub(crate) fn installation_matches(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    installation_id: &str,
) -> bool {
    let Some(record) = ledger.items.get(installation_id) else {
        return false;
    };
    record.binding_ids.iter().all(|binding_id| {
        ledger.bindings.get(binding_id).is_some_and(|binding| {
            binding.resource_ids.iter().all(|resource_id| {
                ledger
                    .resources
                    .get(resource_id)
                    .is_some_and(|resource| resource_matches(paths, resource).unwrap_or(false))
            })
        })
    })
}

pub(crate) fn resource_matches(
    paths: &SystemPaths,
    resource: &ResourceRecord,
) -> Result<bool, String> {
    match &resource.owned {
        OwnedResource::Path(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.path))?;
            if !path_entry_exists(&path) {
                return Ok(false);
            }
            ledger::path_digest(&path, owned.kind).map(|digest| digest == owned.installed_digest)
        }
        OwnedResource::StructuredEntry(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.document_path))?;
            let contents = managed_documents::read_or_empty(&path, owned.format)?;
            let value = managed_documents::entry_value(&contents, owned.format, &owned.key_path)?;
            value
                .as_ref()
                .map(managed_documents::value_digest)
                .transpose()
                .map(|digest| digest.as_deref() == Some(owned.value_digest.as_str()))
        }
        OwnedResource::TextBlock(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.document_path))?;
            let contents = fs::read(&path).unwrap_or_default();
            managed_documents::text_block_body(&contents, &owned.marker_id).map(|body| {
                body.as_deref()
                    .map(|body| ledger::bytes_digest(body.as_bytes()))
                    == Some(owned.body_digest.clone())
            })
        }
    }
}

pub(crate) fn plan_satisfied(
    ledger: &InstallationLedger,
    plan: &OperationPlan,
) -> Result<bool, String> {
    if plan
        .bindings
        .keys()
        .any(|binding_id| !ledger.bindings.contains_key(binding_id))
    {
        return Ok(false);
    }
    for planned in plan.resources.values() {
        let Some(existing) = ledger.resource_by_identity(&planned.desired.identity()) else {
            return Ok(false);
        };
        if existing.desired_digest != planned.desired.desired_digest()? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn plan_matches_ledger(
    ledger: &InstallationLedger,
    plan: &OperationPlan,
) -> Result<bool, String> {
    plan_satisfied(ledger, plan)
}
