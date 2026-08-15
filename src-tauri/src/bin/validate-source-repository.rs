fn main() {
    match parse_args(std::env::args().skip(1).collect()) {
        Ok(input) => finish(if input.starts_with("https://") {
            skill_manager_lib::validate_source_repository_locator(&input)
        } else {
            skill_manager_lib::validate_source_repository(&input)
        }),
        Err(()) => usage(),
    }
}

fn parse_args(args: Vec<String>) -> Result<String, ()> {
    match args.as_slice() {
        [input] => Ok(input.clone()),
        _ => Err(()),
    }
}

fn finish(result: Result<skill_manager_lib::RepositoryValidationReport, String>) {
    match result {
        Ok(report) => {
            println!(
                "{}: {} listed source(s), {} error(s)",
                report.repository_id,
                report.listed_sources,
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

fn usage() -> ! {
    eprintln!("usage: validate-source-repository PATH-OR-HTTPS-URL");
    std::process::exit(2);
}
