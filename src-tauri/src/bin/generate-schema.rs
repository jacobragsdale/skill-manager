use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../schemas");
    let schema = skill_manager_lib::manifest::source_manifest_schema_json()
        .expect("generate source manifest schema");
    for version in ["v1", "v2"] {
        let output = root.join(version).join("source-manifest.schema.json");
        std::fs::create_dir_all(output.parent().expect("schema parent"))
            .expect("create schema directory");
        std::fs::write(&output, &schema).expect("write source manifest schema");
        println!("Generated {}", output.display());
    }
}
