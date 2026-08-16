use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../schemas");
    let source = skill_manager_lib::manifest::source_manifest_schema_json()
        .expect("generate source manifest schema");
    // v1 is a publisher compatibility alias of the current (v2) source-manifest schema.
    for version in ["v1", "v2"] {
        let output = root.join(version).join("source-manifest.schema.json");
        std::fs::create_dir_all(output.parent().expect("schema parent"))
            .expect("create schema directory");
        std::fs::write(&output, &source).expect("write source manifest schema");
        println!("Generated {}", output.display());
    }
    let repository = skill_manager_lib::repository::source_repository_schema_json()
        .expect("generate source repository schema");
    let output = root.join("v1").join("source-repository.schema.json");
    std::fs::create_dir_all(output.parent().expect("schema parent"))
        .expect("create schema directory");
    std::fs::write(&output, repository).expect("write source repository schema");
    println!("Generated {}", output.display());
}
