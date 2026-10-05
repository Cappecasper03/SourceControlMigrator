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

macro_rules! log_verbose {
    ($($arg:tt)*) => {
        if VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[verbose] {}", format_args!($($arg)*));
        }
    };
}

fn hash_url(url: &str) -> String {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(url.as_bytes());
    let id = hasher
        .try_finalize()
        .expect("hashing a short string cannot fail");
    id.to_hex_with_len(16).to_string()
}

fn cache_directory(url: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("SourceControlMigrator")
        .join(hash_url(url))
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
    log_verbose!("Fetching updates into {}", repo.git_dir().display());
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
        log_verbose!("Removing existing directory {}", destination.display());
        std::fs::remove_dir_all(destination)?;
    }
    log_verbose!("Cloning {url} into {}", destination.display());
    std::fs::create_dir_all(destination)?;

    let (root, renderer) = progress_root();
    let mut prepare = gix::prepare_clone_bare(url, destination)?;
    let result = prepare.fetch_only(root.add_child("clone"), &gix::interrupt::IS_INTERRUPTED);
    drop(renderer);
    match result {
        Ok((repo, _outcome)) => Ok(repo),
        Err(error) => {
            eprintln!("gix clone failed: {}", error_chain(&error));
            eprintln!("Falling back to system git");
            clone_with_git(url, destination)
        }
    }
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(&format!("\n  caused by: {cause}"));
        source = cause.source();
    }
    message
}

fn clone_with_git(
    url: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    let status = std::process::Command::new("git")
        .args(["clone", "--bare", "--progress", url])
        .arg(destination)
        .status()?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(destination);
        return Err(format!("git clone failed with {status}").into());
    }
    Ok(gix::open(destination)?)
}

fn fetch_or_clone(
    url: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    if destination.join("HEAD").exists() {
        log_verbose!("Found cached repository at {}", destination.display());
        match gix::open(destination) {
            Ok(repo) => match repo.head_id() {
                Ok(head) => {
                    log_verbose!("Cached repository HEAD is {head}");
                    if let Err(error) = fetch(&repo) {
                        eprintln!("Fetch failed ({error}), using cached repository");
                    } else {
                        log_verbose!("Fetch completed");
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

fn collect_commit_ids(
    repo: &gix::Repository,
) -> Result<Vec<gix::ObjectId>, Box<dyn std::error::Error>> {
    let head = repo.head_id()?;
    let walk = Builder::new(&repo.objects)
        .with_tips([head.detach()])
        .sorting(Sorting::DateOrder)
        .build()
        .map_err(Exn::into_error)?;

    let mut ids = Vec::new();
    for info in walk {
        ids.push(info.map_err(Exn::into_error)?.id);
    }
    ids.reverse();
    log_verbose!("Collected {} commits", ids.len());
    Ok(ids)
}

fn print_commit(
    repo: &gix::Repository,
    id: gix::ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let commit = repo.find_commit(id)?;
    let author = commit.author().map_err(Exn::into_error)?;
    println!(
        "{} {} {}",
        id,
        author.time().map_err(Exn::into_error)?.seconds,
        commit.message().map_err(Exn::into_error)?.summary()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    VERBOSE.store(arguments.verbose, std::sync::atomic::Ordering::Relaxed);
    log_verbose!("URL: {}", arguments.url);
    log_verbose!("Destination argument: {}", arguments.destination);

    let destination = cache_directory(&arguments.url);
    log_verbose!("Cache directory: {}", destination.display());
    let repo = fetch_or_clone(&arguments.url, &destination)?;

    for id in collect_commit_ids(&repo)? {
        print_commit(&repo, id)?;
    }
    Ok(())
}
