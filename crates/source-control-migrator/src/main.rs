mod migrator;
mod source;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Arguments {
    #[arg(short, long, default_value = "tests/repository")]
    source: String,

    #[arg(short, long, default_value = ".")]
    destination: String,

    #[arg(short, long, default_value = "true")]
    force: bool,
}

fn get_source_name(source: &str) -> Option<&str> {
    let is_separator = |c: char| c == '/' || c == '\\' || c == ':';

    let last = source
        .trim_end_matches(is_separator)
        .rsplit(is_separator)
        .next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);

    if name.is_empty() { None } else { Some(name) }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();

    let mut destination = std::path::absolute(&arguments.destination)?;
    if std::path::Path::new(&arguments.destination) == std::path::Path::new(".") {
        let name = get_source_name(&arguments.source).ok_or_else(|| {
            format!(
                "Could not determine a name from source '{}'",
                arguments.source
            )
        })?;
        destination.push(name);
    }

    let destination = destination.as_path();
    if !arguments.force && destination.exists() {
        return Err(format!("Destination '{}' already exists", destination.display()).into());
    }

    let repository = source::open(&arguments.source)?;

    if arguments.force && destination.exists() {
        println!("Removing existing destination: '{}'", destination.display());
        std::fs::remove_dir_all(destination)?;
    }

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    let builder = builder.enable_all();
    let builder = builder.build()?;
    builder.block_on(migrator::migrate_to(&repository, destination))?;

    Ok(())
}
