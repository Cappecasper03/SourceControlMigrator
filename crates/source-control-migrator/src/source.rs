use gix::Exn;
use gix::traverse::commit::topo::{Builder, Sorting};

use crate::Error;

fn hash_url(url: &str) -> String {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(url.as_bytes());
    let id = hasher
        .try_finalize()
        .expect("hashing a short string cannot fail");
    id.to_hex_with_len(16).to_string()
}

pub fn cache_directory(url: &str) -> std::path::PathBuf {
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

fn fetch(repo: &gix::Repository) -> Result<(), Error> {
    log_verbose!("Fetching updates into {}", repo.git_dir().display());
    let (root, renderer) = progress_root();
    let result = (|| -> Result<(), Error> {
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

fn clone(url: &str, destination: &std::path::Path) -> Result<gix::Repository, Error> {
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
        Err(error) => Err(error.into()),
    }
}

pub fn fetch_or_clone(url: &str, destination: &std::path::Path) -> Result<gix::Repository, Error> {
    if destination.join("HEAD").exists() {
        log_verbose!("Found cached repository at {}", destination.display());
        match gix::open(destination) {
            Ok(repo) => {
                if let Err(error) = fetch(&repo) {
                    eprintln!("Fetch failed ({error}), using cached repository");
                } else {
                    log_verbose!("Fetch completed");
                }
                if has_branches(&repo) {
                    return Ok(repo);
                }
                eprintln!("Cached repository has no branches, recloning");
            }
            Err(error) => eprintln!("Cached repository is broken ({error}), recloning"),
        }
    }

    clone(url, destination)
}

/// Brings the repository `source` points at into the cache directory and opens it there. A
/// URL is cloned once and fetched afterwards. A path to a local repository is cloned afresh
/// every time, so the migration never touches or depends on the original working directory.
pub fn open_source(source: &str) -> Result<gix::Repository, Error> {
    let path = std::path::Path::new(source);
    if path.is_dir() {
        let path = std::fs::canonicalize(path)?;
        let path = path.to_string_lossy();
        let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
        return clone(path, &cache_directory(path));
    }
    fetch_or_clone(source, &cache_directory(source))
}

/// A branch of the source repository together with the commits only it brings in.
#[derive(Debug, Clone)]
pub struct BranchPlan {
    pub name: String,
    pub tip: gix::ObjectId,
    /// Commits reachable from `tip` that no earlier planned branch already contains, oldest first.
    pub commits: Vec<gix::ObjectId>,
}

fn branch_tips(repo: &gix::Repository) -> Result<Vec<(String, gix::ObjectId)>, Error> {
    let mut local = std::collections::BTreeMap::new();
    let mut remote = std::collections::BTreeMap::new();
    for reference in repo.references()?.all()? {
        let mut reference = reference?;
        let name = reference.name().as_bstr().to_string();
        let (branches, short) = if let Some(short) = name.strip_prefix("refs/heads/") {
            (&mut local, short)
        } else if let Some(short) = name.strip_prefix("refs/remotes/origin/") {
            (&mut remote, short)
        } else {
            continue;
        };
        if short == "HEAD" {
            continue;
        }
        let short = short.to_string();
        let Ok(id) = reference.peel_to_id() else {
            continue;
        };
        branches.insert(short, id.detach());
    }
    for (name, id) in remote {
        local.entry(name).or_insert(id);
    }
    Ok(local.into_iter().collect())
}

fn has_branches(repo: &gix::Repository) -> bool {
    branch_tips(repo).is_ok_and(|branches| !branches.is_empty())
}

fn default_branch_name(repo: &gix::Repository, names: &[&str]) -> String {
    if let Ok(Some(head)) = repo.head_name() {
        let head = head.shorten().to_string();
        if names.contains(&head.as_str()) {
            return head;
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|name| names.contains(name))
        .unwrap_or(names[0])
        .to_string()
}

/// Splits the history of every branch into the commits each one contributes. The default branch
/// comes first and owns everything it can reach; every other branch, in name order, owns what
/// is left. Commits only reachable from branches that were never merged are therefore migrated.
pub fn plan_branches(repo: &gix::Repository) -> Result<Vec<BranchPlan>, Error> {
    let mut tips = branch_tips(repo)?;
    if tips.is_empty() {
        return Err("the source repository has no branches".into());
    }
    let default = default_branch_name(
        repo,
        &tips
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
    );
    tips.sort_by_key(|(name, _)| (*name != default, name.clone()));
    log_verbose!("Default branch is {default}, found {} branches", tips.len());

    let mut plans = Vec::new();
    let mut seen_tips: Vec<gix::ObjectId> = Vec::new();
    for (name, tip) in tips {
        let walk = Builder::new(&repo.objects)
            .with_tips([tip])
            .with_ends(seen_tips.iter().copied())
            .sorting(Sorting::DateOrder)
            .build()
            .map_err(Exn::into_error)?;
        let mut commits = Vec::new();
        for info in walk {
            commits.push(info.map_err(Exn::into_error)?.id);
        }
        commits.reverse();
        log_verbose!("Branch {name} contributes {} commits", commits.len());
        seen_tips.push(tip);
        plans.push(BranchPlan { name, tip, commits });
    }
    Ok(plans)
}
