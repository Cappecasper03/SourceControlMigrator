use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gix::Exn;
use lore::interface::{LoreArray, LoreEvent, LoreGlobalArgs, LoreMetadataType, LoreString};

use crate::Error;
use crate::source::{BranchPlan, plan_branches};

/// The name Lore gives the branch a new repository starts on.
pub const LORE_DEFAULT_BRANCH: &str = "main";

/// How Lore treats a path that only changed case: the repository follows the new spelling.
const CASE_CHANGE_RENAME: u32 = 2;

/// Where a source commit ended up in the Lore repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub branch: String,
    pub hash: String,
}

#[derive(Debug, Default)]
pub struct Migration {
    /// Source branch name and the Lore branch it was migrated to, default branch first.
    pub branches: Vec<(String, String)>,
    /// The Lore revision every source commit was migrated to.
    pub revisions: HashMap<gix::ObjectId, Revision>,
    /// Commits that changed no file contents and were migrated as empty revisions.
    pub empty: Vec<gix::ObjectId>,
}

type Files = HashMap<PathBuf, gix::ObjectId>;

/// Globals for working on the local repository only, which never talks to a server.
pub fn lore_globals(destination: &Path) -> LoreGlobalArgs {
    LoreGlobalArgs {
        repository_path: LoreString::from(destination.to_string_lossy().as_ref()),
        offline: 1,
        ..Default::default()
    }
}

/// Globals for the calls that have to reach the server.
pub fn lore_online_globals(destination: &Path) -> LoreGlobalArgs {
    LoreGlobalArgs {
        offline: 0,
        ..lore_globals(destination)
    }
}

fn check(status: i32, what: &str) -> Result<(), Error> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("lore {what} failed with status {status}").into())
    }
}

fn report_error(event: &LoreEvent) {
    match event {
        LoreEvent::Error(data) => eprintln!(
            "lore error {}: {}",
            data.error_type,
            data.error_inner.as_str()
        ),
        LoreEvent::Complete(data) if data.status != 0 => eprintln!(
            "lore failed with {}: {}",
            data.error.error_code,
            data.error.message.as_str()
        ),
        _ => {}
    }
}

async fn create_lore_repository(destination: &Path, remote: Option<&str>) -> Result<(), Error> {
    if destination.join(".lore").exists() {
        return Err(format!(
            "{} already contains a Lore repository, refusing to import into it",
            destination.display()
        )
        .into());
    }
    std::fs::create_dir_all(destination)?;
    let args = lore::repository::LoreRepositoryCreateArgs {
        repository_url: LoreString::from(remote.unwrap_or_default()),
        description: LoreString::default(),
        id: LoreString::default(),
        vfs: lore::repository::LoreVfsType::default(),
        use_shared_store: lore::interface::LoreSharedStoreMode::Disabled,
        shared_store_path: LoreString::default(),
    };
    // Registering the repository on a server is the one thing that cannot be done offline.
    let globals = if remote.is_some() {
        lore_online_globals(destination)
    } else {
        lore_globals(destination)
    };
    check(
        lore::repository::create(globals, args, Some(Box::new(report_error) as Box<_>)).await,
        "repository create",
    )
}

async fn push_lore_branch(destination: &Path, branch: &str) -> Result<(), Error> {
    let args = lore::branch::LoreBranchPushArgs {
        branch: LoreString::from(branch),
        fast_forward_merge: 0,
    };
    check(
        lore::branch::push(
            lore_online_globals(destination),
            args,
            Some(Box::new(report_error) as Box<_>),
        )
        .await,
        "branch push",
    )
}

async fn switch_lore_branch(destination: &Path, branch: &str, revision: &str) -> Result<(), Error> {
    let args = lore::branch::LoreBranchSwitchArgs {
        branch: LoreString::from(branch),
        revision: LoreString::from(revision),
        reset: 0,
        bare: 0,
    };
    check(
        lore::branch::switch(
            lore_globals(destination),
            args,
            Some(Box::new(report_error) as Box<_>),
        )
        .await,
        "branch switch",
    )
}

async fn create_lore_branch(destination: &Path, branch: &str) -> Result<(), Error> {
    let args = lore::branch::LoreBranchCreateArgs {
        branch: LoreString::from(branch),
        category: LoreString::default(),
        id: LoreString::default(),
    };
    check(
        lore::branch::create(
            lore_globals(destination),
            args,
            Some(Box::new(report_error) as Box<_>),
        )
        .await,
        "branch create",
    )
}

struct CommitMetadata {
    timestamp_millis: u64,
    author: String,
    committer: String,
}

fn identity(signature: gix::actor::SignatureRef<'_>) -> String {
    format!("{} <{}>", signature.name, signature.email)
}

fn commit_metadata(commit: &gix::Commit<'_>) -> Result<CommitMetadata, Error> {
    let author = commit.author().map_err(Exn::into_error)?;
    let seconds = author.time().map_err(Exn::into_error)?.seconds;
    Ok(CommitMetadata {
        timestamp_millis: u64::try_from(seconds)? * 1000,
        author: identity(author),
        committer: identity(commit.committer().map_err(Exn::into_error)?),
    })
}

fn tree_entries(
    repo: &gix::Repository,
    destination: &Path,
    tree_id: gix::ObjectId,
) -> Result<Files, Error> {
    let tree = repo.find_tree(tree_id)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder)?;
    Ok(recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob() || entry.mode.is_link())
        .map(|entry| {
            (
                destination.join(gix::path::from_bstr(&entry.filepath)),
                entry.oid,
            )
        })
        .collect())
}

/// Brings the working tree from `previous_files` to `tree_id`, touching only files that differ,
/// and returns the paths that were added, modified or removed.
fn checkout_tree(
    repo: &gix::Repository,
    destination: &Path,
    tree_id: gix::ObjectId,
    previous_files: &mut Files,
) -> Result<Vec<PathBuf>, Error> {
    let files = tree_entries(repo, destination, tree_id)?;

    // Removals come first: a file may turn into a directory, a directory into a file, and on a
    // case-insensitive file system a rename that only changes case must not delete the new name.
    let mut changed = Vec::new();
    for removed in previous_files
        .keys()
        .filter(|path| !files.contains_key(*path))
    {
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

    for (path, oid) in &files {
        if previous_files.get(path) == Some(oid) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &repo.find_blob(*oid)?.data)?;
        changed.push(path.clone());
    }
    *previous_files = files;
    Ok(changed)
}

/// Name of the scratch file used to get Lore to commit a revision without changes.
const SCRATCH_FILE: &str = ".source-control-migrator-empty-commit";

/// Lore only commits when the staged state differs from the current one, even forced. Staging a
/// scratch file and unstaging it again leaves such a state behind without changing any content.
async fn stage_nothing(destination: &Path) -> Result<(), Error> {
    let scratch = destination.join(SCRATCH_FILE);
    std::fs::write(&scratch, b"")?;
    let paths = || LoreArray::from_vec(vec![LoreString::from(scratch.to_string_lossy().as_ref())]);
    let staged = lore::file::stage(
        lore_globals(destination),
        lore::file::LoreFileStageArgs {
            paths: paths(),
            case_change: 0,
            scan: 0,
        },
        Some(Box::new(report_error) as Box<_>),
    )
    .await;
    let unstaged = lore::file::unstage(
        lore_globals(destination),
        lore::file::LoreFileUnstageArgs { paths: paths() },
        Some(Box::new(report_error) as Box<_>),
    )
    .await;
    std::fs::remove_file(&scratch)?;
    check(staged, "stage")?;
    check(unstaged, "unstage")
}

/// Stages `changed` and commits it, returning the signature of the new revision. Lore refuses
/// to commit a tree identical to the current one unless forced, which is also what a rename
/// that only changes case or a commit without changes amounts to. Even forced it needs a staged
/// state that differs from the current one, see [`stage_nothing`].
async fn commit_to_lore(
    destination: &Path,
    changed: &[PathBuf],
    message: &str,
    metadata: &CommitMetadata,
) -> Result<String, Error> {
    // Our own diff can miss changes Lore tracks (file modes, say), so with nothing to point at
    // Lore is asked to scan the whole repository instead of trusting that nothing changed.
    // Scanning also makes Lore notice a directory that turned into a file, which it keeps
    // treating as a directory otherwise. It costs nothing for paths that are plain files.
    let paths = if changed.is_empty() {
        vec![LoreString::from(destination.to_string_lossy().as_ref())]
    } else {
        changed
            .iter()
            .map(|path| LoreString::from(path.to_string_lossy().as_ref()))
            .collect()
    };
    let stage = lore::file::LoreFileStageArgs {
        paths: LoreArray::from_vec(paths),
        case_change: CASE_CHANGE_RENAME,
        scan: 1,
    };
    check(
        lore::file::stage(
                lore_globals(destination),
                stage,
                Some(Box::new(report_error) as Box<_>),
            )
            .await,
        "stage",
    )?;
    if changed.is_empty() {
        stage_nothing(destination).await?;
    }

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
    .map(|(key, value, format)| (LoreString::from(key), LoreString::from(value), format))
    .collect();
    let commit = lore::revision::LoreRevisionCommitWithMetadataArgs {
        message: LoreString::from(message),
        keys: LoreArray::from_vec(keys),
        values: LoreArray::from_vec(values),
        formats: LoreArray::from_vec(formats),
    };

    let revision = Arc::new(Mutex::new(None));
    let captured = revision.clone();
    let callback = move |event: &LoreEvent| {
        report_error(event);
        if let LoreEvent::RevisionCommitRevision(data) = event {
            *captured.lock().unwrap() = Some(data.revision.to_string());
        }
    };
    check(
        lore::revision::commit_with_metadata(
            LoreGlobalArgs {
                force: 1,
                ..lore_globals(destination)
            },
            commit,
            Some(Box::new(callback) as Box<_>),
        )
        .await,
        "commit",
    )?;
    let revision = revision.lock().unwrap().take();
    revision.ok_or_else(|| "lore did not report the committed revision".into())
}

/// The name a source branch gets in Lore. The default branch becomes the branch Lore starts
/// with, so a different source branch that happens to be called that needs another name.
fn lore_branch_name(index: usize, name: &str) -> String {
    if index == 0 {
        LORE_DEFAULT_BRANCH.to_string()
    } else if name == LORE_DEFAULT_BRANCH {
        format!("{name}-source")
    } else {
        name.to_string()
    }
}

/// The commit whose Lore revision a branch has to be started from, unless it starts the history.
fn base_commit(
    repo: &gix::Repository,
    index: usize,
    plan: &BranchPlan,
    first_commit: Option<gix::ObjectId>,
) -> Result<Option<gix::ObjectId>, Error> {
    let Some(&first) = plan.commits.first() else {
        return Ok(Some(plan.tip));
    };
    let parent = repo
        .find_commit(first)?
        .parent_ids()
        .next()
        .map(|id| id.detach());
    // A branch with an unrelated history is started from the very first commit instead.
    Ok(parent.or(if index == 0 { None } else { first_commit }))
}

/// Migrates the history of every branch of `repo` into a new Lore repository at `destination`,
/// creating one Lore commit per source commit.
pub async fn migrate(repo: &gix::Repository, destination: &Path) -> Result<Migration, Error> {
    migrate_to(repo, destination, None).await
}

/// Like [`migrate`], and when given the `lore://` URL of a repository on a Lore server, also
/// registers the repository there and pushes every branch to it.
pub async fn migrate_to(
    repo: &gix::Repository,
    destination: &Path,
    remote: Option<&str>,
) -> Result<Migration, Error> {
    let plans = plan_branches(repo)?;
    create_lore_repository(destination, remote).await?;
    let destination = &std::fs::canonicalize(destination)?;

    let mut migration = Migration::default();
    let first_commit = plans.first().and_then(|plan| plan.commits.first().copied());
    let total: usize = plans.iter().map(|plan| plan.commits.len()).sum();
    let mut number = 0;

    for (index, plan) in plans.iter().enumerate() {
        let lore_name = lore_branch_name(index, &plan.name);
        log_verbose!("Migrating branch {} as {lore_name}", plan.name);

        let mut files = Files::new();
        if let Some(base) = base_commit(repo, index, plan, first_commit)? {
            let base_revision = migration
                .revisions
                .get(&base)
                .ok_or_else(|| format!("no Lore revision exists for {base}"))?
                .clone();
            switch_lore_branch(destination, &base_revision.branch, &base_revision.hash).await?;
            if index > 0 {
                create_lore_branch(destination, &lore_name).await?;
            }
            let tree = repo.find_commit(base)?.tree_id().map_err(Exn::into_error)?;
            files = tree_entries(repo, destination, tree.detach())?;
        }

        for &id in &plan.commits {
            number += 1;
            let commit = repo.find_commit(id)?;
            let tree = commit.tree_id().map_err(Exn::into_error)?.detach();
            let changed = checkout_tree(repo, destination, tree, &mut files)?;
            if changed.is_empty() {
                log_verbose!(
                    "[{number}/{total}] {id} changes no file contents, Lore gets to scan for changes"
                );
                migration.empty.push(id);
            }

            let message = commit.message_raw().map_err(Exn::into_error)?.to_string();
            let metadata = commit_metadata(&commit)?;
            let hash = commit_to_lore(
                destination,
                &changed,
                message.trim_end(),
                &metadata,
            )
            .await?;
            migration.revisions.insert(
                id,
                Revision {
                    branch: lore_name.clone(),
                    hash,
                },
            );
            log_verbose!("[{number}/{total}] Migrated {id}");
        }
        if remote.is_some() {
            log_verbose!("Pushing {lore_name}");
            push_lore_branch(destination, &lore_name).await?;
        }
        migration.branches.push((plan.name.clone(), lore_name));
    }

    check(
        lore::repository::flush(
            lore_globals(destination),
            lore::repository::LoreRepositoryFlushArgs {},
            None,
        )
        .await,
        "flush",
    )?;
    Ok(migration)
}
