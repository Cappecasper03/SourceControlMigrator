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
    source: String,

    #[arg(short, long, default_value = "./Test")]
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
        Err(error) => Err(error_chain(&error).into()),
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

type Files = std::collections::HashMap<std::path::PathBuf, gix::ObjectId>;

/// Brings the working tree to `tree_id`, touching only files that differ from `previous_files`,
/// and returns the paths that were added, modified or removed.
fn checkout_commit(
    repo: &gix::Repository,
    destination: &std::path::Path,
    tree_id: gix::ObjectId,
    previous_files: &mut Files,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let tree = repo.find_tree(tree_id)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder)?;

    let mut changed = Vec::new();
    let mut files = Files::with_capacity(previous_files.len());
    for entry in recorder.records {
        if !entry.mode.is_blob() && !entry.mode.is_link() {
            continue;
        }
        let path = destination.join(gix::path::from_bstr(&entry.filepath));
        if previous_files.get(&path) != Some(&entry.oid) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &repo.find_blob(entry.oid)?.data)?;
            changed.push(path.clone());
        }
        files.insert(path, entry.oid);
    }

    for removed in previous_files.keys().filter(|path| !files.contains_key(*path)) {
        std::fs::remove_file(removed)?;
        let mut directory = removed.parent();
        while let Some(parent) = directory.filter(|parent| *parent != destination) {
            if std::fs::remove_dir(parent).is_err() {
                break;
            }
            directory = parent.parent();
        }
        changed.push(removed.clone());
    }
    *previous_files = files;
    Ok(changed)
}

fn lore_globals(destination: &std::path::Path) -> lore::interface::LoreGlobalArgs {
    lore::interface::LoreGlobalArgs {
        repository_path: lore::interface::LoreString::from(destination.to_string_lossy().as_ref()),
        offline: 1,
        ..Default::default()
    }
}

fn check(status: i32, what: &str) -> Result<(), Box<dyn std::error::Error>> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("lore {what} failed with status {status}").into())
    }
}

async fn create_lore_repository(
    destination: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if destination.join(".lore").exists() {
        return Err(format!(
            "{} already contains a Lore repository, refusing to import into it",
            destination.display()
        )
        .into());
    }
    std::fs::create_dir_all(destination)?;
    let args = lore::repository::LoreRepositoryCreateArgs {
        repository_url: lore::interface::LoreString::default(),
        description: lore::interface::LoreString::default(),
        id: lore::interface::LoreString::default(),
        vfs: lore::repository::LoreVfsType::default(),
        use_shared_store: lore::interface::LoreSharedStoreMode::Disabled,
        shared_store_path: lore::interface::LoreString::default(),
    };
    check(
        lore::repository::create(lore_globals(destination), args, None).await,
        "repository create",
    )
}

fn report_event(event: &lore::interface::LoreEvent) {
    if let lore::interface::LoreEvent::Error(data) = event {
        eprintln!(
            "lore error {}: {}",
            data.error_type,
            data.error_inner.as_str()
        );
    }
}

use lore::interface::LoreMetadataType;

struct CommitMetadata {
    timestamp_millis: u64,
    author: String,
    committer: String,
}

fn identity(signature: gix::actor::SignatureRef<'_>) -> String {
    format!("{} <{}>", signature.name, signature.email)
}

fn commit_metadata(commit: &gix::Commit<'_>) -> Result<CommitMetadata, Box<dyn std::error::Error>> {
    let author = commit.author().map_err(Exn::into_error)?;
    let seconds = author.time().map_err(Exn::into_error)?.seconds;
    Ok(CommitMetadata {
        timestamp_millis: u64::try_from(seconds)? * 1000,
        author: identity(author),
        committer: identity(commit.committer().map_err(Exn::into_error)?),
    })
}

async fn commit_to_lore(
    destination: &std::path::Path,
    changed: &[std::path::PathBuf],
    message: &str,
    metadata: &CommitMetadata,
) -> Result<(), Box<dyn std::error::Error>> {
    let stage = lore::file::LoreFileStageArgs {
        paths: lore::interface::LoreArray::from_vec(
            changed
                .iter()
                .map(|path| lore::interface::LoreString::from(path.to_string_lossy().as_ref()))
                .collect(),
        ),
        case_change: 0,
        scan: 0,
    };
    check(
        lore::file::stage(lore_globals(destination), stage, None).await,
        "stage",
    )?;
    let (keys, values, formats): (Vec<_>, Vec<_>, Vec<_>) = [
        (
            "timestamp",
            metadata.timestamp_millis.to_string(),
            LoreMetadataType::Numeric,
        ),
        (
            "created-by",
            metadata.author.clone(),
            LoreMetadataType::String,
        ),
        (
            "committed-by",
            metadata.committer.clone(),
            LoreMetadataType::String,
        ),
    ]
    .into_iter()
    .map(|(key, value, format)| {
        (
            lore::interface::LoreString::from(key),
            lore::interface::LoreString::from(value),
            format,
        )
    })
    .collect();
    let commit = lore::revision::LoreRevisionCommitWithMetadataArgs {
        message: lore::interface::LoreString::from(message),
        keys: lore::interface::LoreArray::from_vec(keys),
        values: lore::interface::LoreArray::from_vec(values),
        formats: lore::interface::LoreArray::from_vec(formats),
    };
    check(
        lore::revision::commit_with_metadata(
            lore_globals(destination),
            commit,
            Some(Box::new(report_event) as Box<_>),
        )
        .await,
        "commit",
    )
}

async fn migrate(
    repo: &gix::Repository,
    ids: Vec<gix::ObjectId>,
    destination: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    create_lore_repository(destination).await?;
    let destination = &std::fs::canonicalize(destination)?;

    let mut files = Files::new();
    let total = ids.len();
    let mut previous_tree = None;
    for (number, id) in ids.into_iter().enumerate() {
        let commit = repo.find_commit(id)?;
        let tree = commit.tree_id().map_err(Exn::into_error)?.detach();
        if previous_tree == Some(tree) {
            log_verbose!(
                "[{}/{total}] {id} has no tree changes, skipping",
                number + 1
            );
            continue;
        }
        previous_tree = Some(tree);

        let message = commit.message_raw().map_err(Exn::into_error)?.to_string();
        let changed = checkout_commit(repo, destination, tree, &mut files)?;
        if changed.is_empty() {
            log_verbose!("[{}/{total}] {id} changes no file contents, skipping", number + 1);
            continue;
        }
        let metadata = commit_metadata(&commit)?;
        commit_to_lore(destination, &changed, message.trim_end(), &metadata).await?;
        log_verbose!("[{}/{total}] Migrated {id}", number + 1);
    }
    check(
        lore::repository::flush(
            lore_globals(destination),
            lore::repository::LoreRepositoryFlushArgs {},
            None,
        )
        .await,
        "flush",
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    VERBOSE.store(arguments.verbose, std::sync::atomic::Ordering::Relaxed);
    log_verbose!("URL: {}", arguments.source);
    log_verbose!("Destination argument: {}", arguments.destination);

    let cache = cache_directory(&arguments.source);
    log_verbose!("Cache directory: {}", cache.display());
    let repo = fetch_or_clone(&arguments.source, &cache)?;
    let ids = collect_commit_ids(&repo)?;

    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(migrate(
            &repo,
            ids,
            std::path::Path::new(&arguments.destination),
        ));
    lore::shutdown();
    result
}
