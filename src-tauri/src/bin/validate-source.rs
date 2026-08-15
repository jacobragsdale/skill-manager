fn main() {
    match parse_args(std::env::args().skip(1).collect()) {
        Ok(input) => finish(if input.starts_with("https://") {
            skill_manager_lib::validate_source_locator(&input)
        } else {
            skill_manager_lib::validate_source(&input)
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

fn finish(result: Result<skill_manager_lib::SourceValidationReport, String>) {
    match result {
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

fn usage() -> ! {
    eprintln!("usage: validate-source PATH-OR-HTTPS-URL");
    std::process::exit(2);
}
