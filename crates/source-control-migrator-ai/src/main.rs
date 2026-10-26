use clap::Parser;
use source_control_migrator_ai::migrate::migrate_to;
use source_control_migrator_ai::source::open_source;
use source_control_migrator_ai::{Error, set_verbose};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Arguments {
    /// A URL to clone or the path of a local repository.
    #[arg(
        short,
        long,
        default_value = "https://github.com/Cappecasper03/SourceControlMigrator.git"
    )]
    source: String,

    #[arg(short, long, default_value = "./target/Test")]
    destination: String,

    /// A `lore://host:port/name` URL of a repository to create on a Lore server and push to.
    #[arg(short, long)]
    remote: Option<String>,

    #[arg(short, long, default_value = "true")]
    verbose: bool,
}

fn main() -> Result<(), Error> {
    let arguments = Arguments::parse();
    set_verbose(arguments.verbose);

    let repo = open_source(&arguments.source)?;

    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(migrate_to(
            &repo,
            std::path::Path::new(&arguments.destination),
            arguments.remote.as_deref(),
        ));
    lore::shutdown();

    let migration = result?;
    for (source, lore) in &migration.branches {
        println!("{source} -> {lore}");
    }
    Ok(())
}
