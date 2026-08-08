#[allow(dead_code)]
#[path = "../manifest.rs"]
mod manifest;
#[path = "../qa_paths.rs"]
mod qa_paths;

use std::path::PathBuf;

fn main() {
    let output =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../schemas/v1/source-manifest.schema.json");
    let schema = manifest::source_manifest_schema_json().expect("generate source manifest schema");
    std::fs::create_dir_all(output.parent().expect("schema parent"))
        .expect("create schema directory");
    std::fs::write(&output, schema).expect("write source manifest schema");
    println!("Generated {}", output.display());
}
