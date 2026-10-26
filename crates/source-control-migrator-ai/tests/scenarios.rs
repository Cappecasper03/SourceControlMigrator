//! Migrates a repository for every history shape the migrator knows about, each as a test of its
//! own, and checks the result against git: one Lore revision per git commit, with the same
//! message, people, time and file contents.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use lore::interface::{LoreEvent, LoreMetadata};
use lore::interface::LoreGlobalArgs;
use source_control_migrator::migrate::{Migration, lore_globals, lore_online_globals, migrate_to};
use source_control_migrator::set_verbose;
use source_control_migrator::source::open_source;

fn git(directory: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_text(directory: &Path, args: &[&str]) -> String {
    String::from_utf8(git(directory, args)).unwrap()
}

struct Expected {
    message: String,
    author: String,
    committer: String,
    timestamp_millis: u64,
}

fn expected(directory: &Path, commit: &str) -> Expected {
    let format = "--format=%an <%ae>%x00%cn <%ce>%x00%at";
    let fields = git_text(directory, &["show", "--no-patch", format, commit]);
    let mut fields = fields.trim_end().split('\0');
    let author = fields.next().unwrap().to_string();
    let committer = fields.next().unwrap().to_string();
    let seconds: u64 = fields.next().unwrap().parse().unwrap();
    let message = git_text(directory, &["show", "--no-patch", "--format=%B", commit]);
    Expected {
        message: message.trim_end().to_string(),
        author,
        committer,
        timestamp_millis: seconds * 1000,
    }
}

/// Every file and link git tracks in `commit`, with the blob it holds. Submodules are not files.
fn git_files(directory: &Path, commit: &str) -> BTreeMap<String, String> {
    let listing = git(directory, &["ls-tree", "-r", "-z", commit]);
    String::from_utf8_lossy(&listing)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mut meta = meta.split(' ');
            let _mode = meta.next()?;
            (meta.next()? == "blob").then(|| (path.to_string(), meta.next().unwrap().to_string()))
        })
        .collect()
}

fn lore_files(source: &Path, destination: &Path) -> BTreeMap<String, String> {
    fn walk(source: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".lore") {
                continue;
            }
            if path.is_dir() {
                walk(source, &path, files);
            } else {
                let relative = path.strip_prefix(source).unwrap();
                let content = std::fs::read(&path).unwrap();
                let hash = gix::objs::compute_hash(
                    gix::hash::Kind::Sha1,
                    gix::object::Kind::Blob,
                    &content,
                )
                .unwrap();
                files.insert(
                    relative.to_string_lossy().replace('\\', "/"),
                    hash.to_string(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(destination, destination, &mut files);
    let _ = source;
    files
}

#[derive(Default)]
struct Info {
    metadata: HashMap<String, String>,
    numbers: HashMap<String, u64>,
}

/// Where a migration is read back from: the local repository, or a clone of what the server got.
#[derive(Clone, Copy)]
struct Reader<'a> {
    directory: &'a Path,
    online: bool,
}

impl Reader<'_> {
    fn globals(&self) -> LoreGlobalArgs {
        if self.online {
            lore_online_globals(self.directory)
        } else {
            lore_globals(self.directory)
        }
    }
}

async fn revision_info(reader: Reader<'_>, revision: &str) -> Info {
    let info = Arc::new(Mutex::new(Info::default()));
    let captured = info.clone();
    let callback = move |event: &LoreEvent| {
        if let LoreEvent::Metadata(data) = event {
            let mut info = captured.lock().unwrap();
            match &data.value {
                LoreMetadata::String(value) => {
                    info.metadata
                        .insert(data.key.as_str().to_string(), value.as_str().to_string());
                }
                LoreMetadata::Numeric(value) => {
                    info.numbers.insert(data.key.as_str().to_string(), *value);
                }
                _ => {}
            }
        }
    };
    let status = lore::revision::info(
        reader.globals(),
        lore::revision::LoreRevisionInfoArgs {
            revision: revision.into(),
            delta: 0,
            metadata: 1,
        },
        Some(Box::new(callback) as Box<_>),
    )
    .await;
    assert_eq!(status, 0, "revision info of {revision}");
    Arc::try_unwrap(info).ok().unwrap().into_inner().unwrap()
}

async fn sync(reader: Reader<'_>, revision: &str) -> Result<(), String> {
    let destination = reader.directory;
    // Moving between revisions on top of the previous one chokes on paths that turn from files
    // into directories, so every revision starts from an empty working directory.
    for entry in std::fs::read_dir(destination).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".lore") {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
    {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let captured = errors.clone();
        let callback = move |event: &LoreEvent| match event {
            LoreEvent::Error(data) => captured
                .lock()
                .unwrap()
                .push(data.error_inner.as_str().to_string()),
            LoreEvent::Complete(data) if data.status != 0 => captured
                .lock()
                .unwrap()
                .push(data.error.message.as_str().to_string()),
            _ => {}
        };
        let status = lore::revision::sync(
            reader.globals(),
            lore::revision::LoreRevisionSyncArgs {
                revision: revision.into(),
                reset: 1,
                ..Default::default()
            },
            Some(Box::new(callback) as Box<_>),
        )
        .await;
        if status != 0 {
            return Err(format!("sync failed: {:?}", errors.lock().unwrap()));
        }
    }
    Ok(())
}

/// Builds the repository of one scenario, migrates it and compares the result with git.
fn check(name: &str, scenario: common::Scenario) -> Migration {
    run(name, scenario, false).unwrap()
}

/// Like [`check`], but pushes to a throwaway Lore server and compares what a clone of the
/// repository gets from it. Does nothing without a `loreserver` to start.
fn check_on_server(name: &str, scenario: common::Scenario) -> Option<Migration> {
    run(name, scenario, true)
}

fn run(name: &str, scenario: common::Scenario, on_server: bool) -> Option<Migration> {
    set_verbose(true);
    let kind = if on_server { "server" } else { "local" };
    let root = std::env::temp_dir().join(format!("scm-{kind}-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let source = root.join("source");
    let destination = root.join("lore");
    common::build_scenario(&source, scenario);

    // The path form of the source goes through the same cache a URL would.
    let repository = open_source(source.to_str().unwrap()).unwrap();
    assert_ne!(
        repository.git_dir().canonicalize().unwrap(),
        source.join(".git").canonicalize().unwrap(),
        "the original working directory must not be used"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = if on_server {
        Some(common::server::shared()?)
    } else {
        None
    };
    let remote = server.map(|server| server.url(name));
    let migration = runtime
        .block_on(migrate_to(&repository, &destination, remote.as_deref()))
        .unwrap();

    // What the server holds is read back through a clone, the way anyone else would get it.
    let clone = root.join("clone");
    let reader = match &remote {
        Some(url) => {
            std::fs::create_dir_all(&clone).unwrap();
            let status = runtime.block_on(lore::repository::clone(
                lore_online_globals(&clone),
                lore::repository::LoreRepositoryCloneArgs {
                    repository_url: url.as_str().into(),
                    ..Default::default()
                },
                None,
            ));
            assert_eq!(status, 0, "cloning {url}");
            Reader { directory: &clone, online: true }
        }
        None => Reader { directory: &destination, online: false },
    };

    let commits: Vec<String> = git_text(&source, &["rev-list", "--all"])
        .lines()
        .map(str::to_string)
        .collect();
    // The default branch comes first and is the one Lore starts on.
    assert_eq!(migration.branches[0], ("master".into(), "main".into()));

    // Every commit has a revision of its own.
    let mut revisions = HashSet::new();
    for commit in &commits {
        let id = gix::ObjectId::from_hex(commit.as_bytes()).unwrap();
        let revision = migration
            .revisions
            .get(&id)
            .unwrap_or_else(|| panic!("commit {commit} was not migrated"));
        assert!(
            revisions.insert(revision.hash.clone()),
            "commit {commit} shares its revision with another commit"
        );
    }

    let mut problems = Vec::new();
    runtime.block_on(async {
        for commit in &commits {
            let id = gix::ObjectId::from_hex(commit.as_bytes()).unwrap();
            let revision = &migration.revisions[&id];
            let subject = git_text(&source, &["show", "--no-patch", "--format=%s", commit]);
            let label = format!("{} {}", &commit[..8], subject.trim());

            let want = expected(&source, commit);
            let info = revision_info(reader, &revision.hash).await;
            let got = |key: &str| info.metadata.get(key).cloned().unwrap_or_default();
            if got("message") != want.message {
                problems.push(format!("{label}: message {:?}", got("message")));
            }
            if got("created-by") != want.author {
                problems.push(format!("{label}: author {:?}", got("created-by")));
            }
            if got("committed-by") != want.committer {
                problems.push(format!("{label}: committer {:?}", got("committed-by")));
            }
            if info.numbers.get("timestamp") != Some(&want.timestamp_millis) {
                problems.push(format!("{label}: timestamp {:?}", info.numbers.get("timestamp")));
            }

            if let Err(error) = sync(reader, &revision.hash).await {
                problems.push(format!("{label}: {error}"));
                continue;
            }
            let files = lore_files(reader.directory, reader.directory);
            let wanted = git_files(&source, commit);
            if files != wanted {
                let missing: Vec<_> = wanted.keys().filter(|path| !files.contains_key(*path)).collect();
                let extra: Vec<_> = files.keys().filter(|path| !wanted.contains_key(*path)).collect();
                let changed: Vec<_> = wanted
                    .iter()
                    .filter(|(path, hash)| files.get(*path).is_some_and(|other| other != *hash))
                    .map(|(path, _)| path)
                    .collect();
                problems.push(format!(
                    "{label}: files differ, missing {missing:?}, extra {extra:?}, changed {changed:?}"
                ));
            }
        }
    });
    if problems.is_empty() {
        let _ = std::fs::remove_dir_all(&root);
    }
    assert!(
        problems.is_empty(),
        "{} problems (kept in {}):\n{}",
        problems.len(),
        root.display(),
        problems.join("\n")
    );
    Some(migration)
}

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name), common::$name);
            }
        )*

        /// The same scenarios, pushed to a Lore server and read back from it.
        mod on_server {
            use super::*;
            $(
                #[test]
                fn $name() {
                    check_on_server(stringify!($name), common::$name);
                }
            )*
        }
    };
}

scenarios!(
    file_kinds,
    edit_and_delete,
    rename_and_move,
    directory_removal,
    revert_to_earlier_tree,
    case_only_rename,
    symbolic_link,
    author_differs_from_committer,
    unicode_and_multiline_messages,
    merge_with_new_content,
    merge_equal_to_second_parent,
    merge_equal_to_first_parent,
    octopus_merge,
    unmerged_branch,
    unrelated_root,
    emptied_tree,
);

/// The scenarios below also care about how Lore was asked to commit.
#[test]
fn empty_commits() {
    let migration = check("empty_commits", common::empty_commits);
    assert_eq!(migration.empty.len(), 2);
}

#[test]
fn mode_only_changes() {
    let migration = check("mode_only_changes", common::mode_only_changes);
    assert_eq!(migration.empty.len(), 2, "modes are not carried over");
}

#[test]
fn submodule() {
    let migration = check("submodule", common::submodule);
    assert_eq!(migration.empty.len(), 1, "submodules are not carried over");
}

#[test]
fn file_becomes_directory() {
    let migration = check("file_becomes_directory", common::file_becomes_directory);
    assert!(migration.empty.is_empty());
}

#[test]
fn branch_named_main() {
    let migration = check("branch_named_main", common::branch_named_main);
    let branches: HashMap<_, _> = migration.branches.iter().cloned().collect();
    assert_eq!(branches["master"], "main");
    assert_eq!(branches["main"], "main-source");
}

/// Every scenario in one repository, the way a real history mixes them.
#[test]
fn all_scenarios_together() {
    let migration = check("all_scenarios_together", |git| {
        for (_, scenario) in common::SCENARIOS {
            scenario(git);
            git.checkout("master");
        }
    });
    assert_eq!(migration.branches[0].1, "main");
}
