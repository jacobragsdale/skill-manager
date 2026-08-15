//! Comment-preserving JSONC/TOML and exact marked-text mutations.

use crate::ledger::bytes_digest;
use crate::resource::StructuredFormat;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item};

pub(crate) fn read_or_empty(path: &Path, format: StructuredFormat) -> Result<Vec<u8>, String> {
    match fs::read(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(match format {
            StructuredFormat::Json | StructuredFormat::Jsonc => b"{}\n".to_vec(),
            StructuredFormat::Toml => Vec::new(),
        }),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

pub(crate) fn entry_value(
    contents: &[u8],
    format: StructuredFormat,
    key_path: &[String],
) -> Result<Option<Value>, String> {
    if key_path.is_empty() {
        return Err("A structured resource must have a non-empty key path.".to_string());
    }
    let value = match format {
        StructuredFormat::Json => serde_json::from_slice::<Value>(contents)
            .map_err(|error| format!("Could not parse JSON configuration: {error}"))?,
        StructuredFormat::Jsonc => {
            let text = std::str::from_utf8(contents)
                .map_err(|error| format!("JSONC configuration is not UTF-8: {error}"))?;
            let root = CstRootNode::parse(text, &ParseOptions::default())
                .map_err(|error| format!("Could not parse JSONC configuration: {error}"))?;
            root.to_serde_value()
                .ok_or_else(|| "JSONC configuration has no root value.".to_string())?
        }
        StructuredFormat::Toml => {
            let text = std::str::from_utf8(contents)
                .map_err(|error| format!("TOML configuration is not UTF-8: {error}"))?;
            if text.trim().is_empty() {
                Value::Object(Map::new())
            } else {
                toml_edit::de::from_str::<Value>(text)
                    .map_err(|error| format!("Could not parse TOML configuration: {error}"))?
            }
        }
    };
    let mut current = &value;
    for key in key_path {
        let Some(next) = current.as_object().and_then(|object| object.get(key)) else {
            return Ok(None);
        };
        current = next;
    }
    Ok(Some(current.clone()))
}

pub(crate) fn set_entries(
    contents: &[u8],
    format: StructuredFormat,
    entries: &[(Vec<String>, Value)],
) -> Result<Vec<u8>, String> {
    match format {
        StructuredFormat::Json => set_json_entries(contents, entries),
        StructuredFormat::Jsonc => set_jsonc_entries(contents, entries),
        StructuredFormat::Toml => set_toml_entries(contents, entries),
    }
}

pub(crate) fn remove_entries(
    contents: &[u8],
    format: StructuredFormat,
    key_paths: &[Vec<String>],
) -> Result<Vec<u8>, String> {
    match format {
        StructuredFormat::Json => remove_json_entries(contents, key_paths),
        StructuredFormat::Jsonc => remove_jsonc_entries(contents, key_paths),
        StructuredFormat::Toml => remove_toml_entries(contents, key_paths),
    }
}

fn set_json_entries(contents: &[u8], entries: &[(Vec<String>, Value)]) -> Result<Vec<u8>, String> {
    let mut root = serde_json::from_slice::<Value>(contents)
        .map_err(|error| format!("Could not parse JSON configuration: {error}"))?;
    for (path, value) in entries {
        set_json_value(&mut root, path, value.clone())?;
    }
    let mut output = serde_json::to_vec_pretty(&root)
        .map_err(|error| format!("Could not serialize JSON configuration: {error}"))?;
    output.push(b'\n');
    Ok(output)
}

fn remove_json_entries(contents: &[u8], key_paths: &[Vec<String>]) -> Result<Vec<u8>, String> {
    let mut root = serde_json::from_slice::<Value>(contents)
        .map_err(|error| format!("Could not parse JSON configuration: {error}"))?;
    for path in key_paths {
        remove_json_value(&mut root, path)?;
    }
    let mut output = serde_json::to_vec_pretty(&root)
        .map_err(|error| format!("Could not serialize JSON configuration: {error}"))?;
    output.push(b'\n');
    Ok(output)
}

fn set_json_value(root: &mut Value, path: &[String], value: Value) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| "A structured resource must have a non-empty key path.".to_string())?;
    let mut current = root;
    for key in parents {
        let object = current
            .as_object_mut()
            .ok_or_else(|| format!("Configuration key {key} is not an object."))?;
        current = object
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    current
        .as_object_mut()
        .ok_or_else(|| format!("Configuration parent for {last} is not an object."))?
        .insert(last.clone(), value);
    Ok(())
}

fn remove_json_value(root: &mut Value, path: &[String]) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| "A structured resource must have a non-empty key path.".to_string())?;
    let mut current = root;
    for key in parents {
        let Some(next) = current
            .as_object_mut()
            .and_then(|object| object.get_mut(key))
        else {
            return Ok(());
        };
        current = next;
    }
    if let Some(object) = current.as_object_mut() {
        object.remove(last);
    }
    Ok(())
}

fn set_jsonc_entries(contents: &[u8], entries: &[(Vec<String>, Value)]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|error| format!("JSONC configuration is not UTF-8: {error}"))?;
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|error| format!("Could not parse JSONC configuration: {error}"))?;
    let root_object = root
        .object_value_or_create()
        .ok_or_else(|| "JSONC configuration root must be an object.".to_string())?;
    for (path, value) in entries {
        let (last, parents) = path
            .split_last()
            .ok_or_else(|| "A structured resource must have a non-empty key path.".to_string())?;
        let mut current = root_object.clone();
        for key in parents {
            current = current
                .object_value_or_create(key)
                .ok_or_else(|| format!("JSONC configuration key {key} is not an object."))?;
        }
        let input = json_input(value)?;
        if let Some(property) = current.get(last) {
            property.set_value(input);
        } else {
            current.append(last, input);
        }
    }
    Ok(root.to_string().into_bytes())
}

fn remove_jsonc_entries(contents: &[u8], key_paths: &[Vec<String>]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|error| format!("JSONC configuration is not UTF-8: {error}"))?;
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|error| format!("Could not parse JSONC configuration: {error}"))?;
    let Some(root_object) = root.object_value_or_create() else {
        return Err("JSONC configuration root must be an object.".to_string());
    };
    for path in key_paths {
        let (last, parents) = path
            .split_last()
            .ok_or_else(|| "A structured resource must have a non-empty key path.".to_string())?;
        let mut current = Some(root_object.clone());
        for key in parents {
            current = current.and_then(|object| object.object_value_or_create(key));
        }
        if let Some(property) = current.and_then(|object| object.get(last)) {
            property.remove();
        }
    }
    Ok(root.to_string().into_bytes())
}

fn json_input(value: &Value) -> Result<CstInputValue, String> {
    match value {
        Value::Null => Ok(CstInputValue::Null),
        Value::Bool(value) => Ok(CstInputValue::Bool(*value)),
        Value::Number(value) => Ok(CstInputValue::Number(value.to_string())),
        Value::String(value) => Ok(CstInputValue::String(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(json_input)
            .collect::<Result<Vec<_>, _>>()
            .map(CstInputValue::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), json_input(value)?)))
            .collect::<Result<Vec<_>, String>>()
            .map(CstInputValue::Object),
    }
}

fn set_toml_entries(contents: &[u8], entries: &[(Vec<String>, Value)]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|error| format!("TOML configuration is not UTF-8: {error}"))?;
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|error| format!("Could not parse TOML configuration: {error}"))?
    };
    for (path, value) in entries {
        set_toml_value(&mut document, path, value)?;
    }
    Ok(document.to_string().into_bytes())
}

fn remove_toml_entries(contents: &[u8], key_paths: &[Vec<String>]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|error| format!("TOML configuration is not UTF-8: {error}"))?;
    let mut document = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|error| format!("Could not parse TOML configuration: {error}"))?
    };
    for path in key_paths {
        remove_toml_path(document.as_table_mut(), path)?;
    }
    Ok(document.to_string().into_bytes())
}

fn remove_toml_path(table: &mut toml_edit::Table, path: &[String]) -> Result<(), String> {
    let Some((first, rest)) = path.split_first() else {
        return Err("A structured resource must have a non-empty key path.".to_string());
    };
    if rest.is_empty() {
        table.remove(first);
        return Ok(());
    }
    let Some(child) = table.get_mut(first).and_then(Item::as_table_mut) else {
        return Ok(());
    };
    remove_toml_path(child, rest)
}

fn set_toml_value(
    document: &mut DocumentMut,
    path: &[String],
    value: &Value,
) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| "A structured resource must have a non-empty key path.".to_string())?;
    let mut table = document.as_table_mut();
    for key in parents {
        if !table.contains_key(key) {
            table.insert(key, Item::Table(toml_edit::Table::new()));
        }
        table = table
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| format!("TOML configuration key {key} is not a table."))?;
    }
    table.insert(last, json_to_toml_item(value)?);
    Ok(())
}

fn json_to_toml_item(value: &Value) -> Result<Item, String> {
    let wrapper = BTreeMap::from([("managed", value)]);
    let mut document = toml_edit::ser::to_document(&wrapper)
        .map_err(|error| format!("Could not serialize a TOML config entry: {error}"))?;
    document
        .as_table_mut()
        .remove("managed")
        .ok_or_else(|| "Could not extract a serialized TOML config entry.".to_string())
}

pub(crate) fn marked_block(body: &str, marker_id: &str) -> String {
    format!(
        "<!-- skill-manager:start:{marker_id} -->\n{}\n<!-- skill-manager:end:{marker_id} -->",
        body.trim_end()
    )
}

pub(crate) fn set_text_blocks(
    contents: &[u8],
    blocks: &[(String, String)],
) -> Result<Vec<u8>, String> {
    let mut text = std::str::from_utf8(contents)
        .map_err(|error| format!("Instructions file is not UTF-8: {error}"))?
        .to_string();
    for (marker_id, body) in blocks {
        text = remove_text_block_string(&text, marker_id)?;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if !text.trim().is_empty() {
            text.push('\n');
        }
        text.push_str(&marked_block(body, marker_id));
        text.push('\n');
    }
    Ok(text.into_bytes())
}

pub(crate) fn remove_text_blocks(
    contents: &[u8],
    marker_ids: &[String],
) -> Result<Vec<u8>, String> {
    let mut text = std::str::from_utf8(contents)
        .map_err(|error| format!("Instructions file is not UTF-8: {error}"))?
        .to_string();
    for marker_id in marker_ids {
        text = remove_text_block_string(&text, marker_id)?;
    }
    Ok(text.into_bytes())
}

pub(crate) fn text_block_body(contents: &[u8], marker_id: &str) -> Result<Option<String>, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|error| format!("Instructions file is not UTF-8: {error}"))?;
    let start = format!("<!-- skill-manager:start:{marker_id} -->");
    let end = format!("<!-- skill-manager:end:{marker_id} -->");
    let Some(start_index) = text.find(&start) else {
        return Ok(None);
    };
    if text[start_index + start.len()..].contains(&start) {
        return Err(format!(
            "Instruction marker {marker_id} appears more than once."
        ));
    }
    let body_start = start_index + start.len();
    let Some(relative_end) = text[body_start..].find(&end) else {
        return Err(format!(
            "Instruction marker {marker_id} has no closing marker."
        ));
    };
    Ok(Some(
        text[body_start..body_start + relative_end]
            .trim_matches('\n')
            .to_string(),
    ))
}

fn remove_text_block_string(text: &str, marker_id: &str) -> Result<String, String> {
    let start = format!("<!-- skill-manager:start:{marker_id} -->");
    let end = format!("<!-- skill-manager:end:{marker_id} -->");
    let Some(start_index) = text.find(&start) else {
        return Ok(text.to_string());
    };
    let body_start = start_index + start.len();
    let relative_end = text[body_start..]
        .find(&end)
        .ok_or_else(|| format!("Instruction marker {marker_id} has no closing marker."))?;
    let mut removal_end = body_start + relative_end + end.len();
    if text[removal_end..].starts_with('\n') {
        removal_end += 1;
    }
    let mut output = String::with_capacity(text.len());
    output.push_str(text[..start_index].trim_end_matches('\n'));
    if !output.is_empty() && !text[removal_end..].trim_start_matches('\n').is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(text[removal_end..].trim_start_matches('\n'));
    if !output.is_empty() && text.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

pub(crate) fn value_digest(value: &Value) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| bytes_digest(&bytes))
        .map_err(|error| format!("Could not serialize a config value for hashing: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jsonc_entry_mutation_preserves_comments_and_unrelated_keys() {
        let original = br#"{
  // keep this explanation
  "theme": "dark",
  "mcp": {}
}
"#;
        let updated = set_entries(
            original,
            StructuredFormat::Jsonc,
            &[(
                vec!["mcp".to_string(), "acme".to_string()],
                json!({"type":"remote","url":"https://example.com"}),
            )],
        )
        .expect("update");
        let text = String::from_utf8(updated).expect("utf8");
        assert!(text.contains("// keep this explanation"));
        assert!(text.contains("\"theme\": \"dark\""));
        assert!(text.contains("\"acme\""));
    }

    #[test]
    fn toml_entry_mutation_preserves_comments_and_unrelated_keys() {
        let original = b"model = \"gpt\" # keep\n";
        let updated = set_entries(
            original,
            StructuredFormat::Toml,
            &[(
                vec!["mcp_servers".to_string(), "acme".to_string()],
                json!({"command":"node","args":["server.js"]}),
            )],
        )
        .expect("update");
        let text = String::from_utf8(updated).expect("utf8");
        assert!(text.contains("model = \"gpt\" # keep"));
        assert!(text.contains("mcp_servers"));
        assert!(text.contains("acme"));
    }

    #[test]
    fn text_blocks_round_trip_without_touching_user_text() {
        let installed = set_text_blocks(b"# Mine\n", &[("acme".to_string(), "Rules".to_string())])
            .expect("install");
        assert_eq!(
            text_block_body(&installed, "acme")
                .expect("body")
                .as_deref(),
            Some("Rules")
        );
        let removed = remove_text_blocks(&installed, &["acme".to_string()]).expect("remove");
        assert_eq!(String::from_utf8(removed).expect("utf8"), "# Mine\n");
    }
}
