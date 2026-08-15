use skill_manager_lib::LocatorKind;

fn main() {
    match parse_args(std::env::args().skip(1).collect()) {
        Ok((None, input)) => finish(skill_manager_lib::validate_source_repository(&input)),
        Ok((Some(kind), input)) => finish(skill_manager_lib::validate_source_repository_locator(
            kind, &input,
        )),
        Err(()) => usage(),
    }
}

fn parse_args(args: Vec<String>) -> Result<(Option<LocatorKind>, String), ()> {
    match args.as_slice() {
        [input] => Ok((None, input.clone())),
        [flag, kind, input] if flag == "--kind" => Ok((Some(parse_kind(kind)?), input.clone())),
        _ => Err(()),
    }
}

fn parse_kind(value: &str) -> Result<LocatorKind, ()> {
    match value {
        "git" => Ok(LocatorKind::Git),
        "artifact" => Ok(LocatorKind::Artifact),
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
    eprintln!("usage: validate-source-repository [--kind git|artifact] PATH-OR-URL");
    std::process::exit(2);
}
