mod source;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Arguments {
    #[arg(short, long, default_value = "https://github.com/Cappecasper03/SourceControlMigrator.git")]
    source: String,

    #[arg(short, long, default_value = ".")]
    destination: String,

    #[arg(short, long)]
    remote: Option<String>,

    #[arg(short, long, default_value = "true")]
    verbose: bool,
}

fn main() {
    let arguments = Arguments::parse();
    let source = source::open_source(&arguments.source);
}
