fn main() {
    let input = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: validate-source REPOSITORY-OR-URL");
        std::process::exit(2);
    });
    match skill_manager_lib::validate_source(&input) {
        Ok(report) => {
            println!(
                "{}: {} valid install(s), {} catalog error(s)",
                report.source_id,
                report.valid_installs,
                report.errors.len()
            );
            for error in report.errors {
                println!("{}: {}", error.path, error.message);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
