use clap::Parser;
use gix::Exn;
use gix::traverse::commit::topo::{Builder, Sorting};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Arguments {
    #[arg(
        short,
        long,
        default_value = "https://github.com/Cappecasper03/SourceControlMigrator.git"
    )]
    url: String,

    #[arg(short, long, default_value = ".")]
    destination: String,

    #[arg(short, long, default_value = "true")]
    verbose: bool,
}

static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

macro_rules! verbose {
    ($($arg:tt)*) => {
        if VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[verbose] {}", format_args!($($arg)*));
        }
    };
}

fn parse_url(url: &str) -> std::path::PathBuf {
    let mut parsed_url = std::path::PathBuf::new();

    for segment in url
        .split(['/', '\\', ':'])
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
    {
        parsed_url.push(segment.replace(
            |c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')),
            "_",
        ));
    }
    parsed_url
}

fn cache_directory(url: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("SourceControlMigrator")
        .join(parse_url(url))
}

fn progress_root() -> (
    std::sync::Arc<prodash::tree::Root>,
    prodash::render::line::JoinHandle,
) {
    let root = prodash::tree::Root::new();
    let renderer = prodash::render::line(
        std::io::stderr(),
        std::sync::Arc::downgrade(&root),
        prodash::render::line::Options {
            frames_per_second: 1.0,
            throughput: true,
            ..Default::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr),
    );
    (root, renderer)
}

fn fetch(repo: &gix::Repository) -> Result<(), Box<dyn std::error::Error>> {
    verbose!("Fetching updates into {}", repo.git_dir().display());
    let (root, renderer) = progress_root();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        repo.find_fetch_remote(None)?
            .with_refspecs(
                ["+refs/heads/*:refs/heads/*"],
                gix::remote::Direction::Fetch,
            )?
            .connect(gix::remote::Direction::Fetch)?
            .prepare_fetch(root.add_child("prepare"), Default::default())?
            .receive(root.add_child("fetch"), &gix::interrupt::IS_INTERRUPTED)?;
        Ok(())
    })();
    drop(renderer);
    result
}

fn clone(
    url: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    if destination.exists() {
        verbose!("Removing existing directory {}", destination.display());
        std::fs::remove_dir_all(destination)?;
    }
    verbose!("Cloning {url} into {}", destination.display());
    std::fs::create_dir_all(destination)?;

    let (root, renderer) = progress_root();
    let mut prepare = gix::prepare_clone_bare(url, destination)?;
    let result = prepare.fetch_only(root.add_child("clone"), &gix::interrupt::IS_INTERRUPTED);
    drop(renderer);
    match result {
        Ok((repo, _outcome)) => Ok(repo),
        Err(error) => {
            let _ = std::fs::remove_dir_all(destination);
            Err(error.into())
        }
    }
}

fn fetch_or_clone(
    url: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    if destination.join("HEAD").exists() {
        verbose!("Found cached repository at {}", destination.display());
        match gix::open(destination) {
            Ok(repo) => match repo.head_id() {
                Ok(head) => {
                    verbose!("Cached repository HEAD is {head}");
                    if let Err(error) = fetch(&repo) {
                        eprintln!("Fetch failed ({error}), using cached repository");
                    } else {
                        verbose!("Fetch completed");
                    }
                    return Ok(repo);
                }
                Err(error) => eprintln!("Cached repository is incomplete ({error}), recloning"),
            },
            Err(error) => eprintln!("Cached repository is broken ({error}), recloning"),
        }
    }

    clone(url, destination)
}

fn collect_commits(
    repo: &gix::Repository,
) -> Result<Vec<gix::traverse::commit::Info>, Box<dyn std::error::Error>> {
    let head = repo.head_id()?;
    let mut commits = Builder::new(&repo.objects)
        .with_tips([head.detach()])
        .sorting(Sorting::DateOrder)
        .build()
        .map_err(Exn::into_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Exn::into_error)?;
    commits.reverse();
    verbose!("Collected {} commits", commits.len());
    Ok(commits)
}

fn print_commit(
    repo: &gix::Repository,
    info: &gix::traverse::commit::Info,
) -> Result<(), Box<dyn std::error::Error>> {
    let commit = repo.find_commit(info.id)?;
    let author = commit.author().map_err(Exn::into_error)?;
    println!(
        "{} {} {}",
        info.id,
        author.time().map_err(Exn::into_error)?.seconds,
        commit.message().map_err(Exn::into_error)?.summary()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    VERBOSE.store(arguments.verbose, std::sync::atomic::Ordering::Relaxed);
    verbose!("URL: {}", arguments.url);
    verbose!("Destination argument: {}", arguments.destination);

    let destination = cache_directory(&arguments.url);
    verbose!("Cache directory: {}", destination.display());
    let repo = fetch_or_clone(&arguments.url, &destination)?;

    for info in collect_commits(&repo)? {
        print_commit(&repo, &info)?;
    }
    Ok(())
}
