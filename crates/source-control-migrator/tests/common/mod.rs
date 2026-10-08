//! Builds a git repository that contains every history shape the migrator has to cope with.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod scenarios;
pub mod server;
pub use scenarios::*;

pub struct Git {
    dir: PathBuf,
    clock: u64,
}

/// 2024-01-01T00:00:00Z, every commit lands a minute after the previous one.
const START: u64 = 1_704_067_200;

impl Git {
    pub fn init(dir: &Path) -> Git {
        std::fs::create_dir_all(dir).unwrap();
        let git = Git {
            dir: dir.to_path_buf(),
            clock: START,
        };
        git.run(&["init", "--quiet", "--initial-branch=master"]);
        git.run(&["config", "user.name", "Alice Author"]);
        git.run(&["config", "user.email", "alice@example.com"]);
        git.run(&["config", "core.autocrlf", "false"]);
        git.run(&["config", "core.symlinks", "false"]);
        git.run(&["config", "commit.gpgsign", "false"]);
        git
    }

    pub fn run(&self, args: &[&str]) -> String {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], envs: &[(&str, String)]) -> String {
        let output = Command::new("git")
            .current_dir(&self.dir)
            .args(args)
            .envs(envs.iter().map(|(key, value)| (*key, value.as_str())))
            .output()
            .expect("git is required to run the scenario tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    pub fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn remove(&self, relative: &str) {
        let path = self.path(relative);
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    pub fn rename(&self, from: &str, to: &str) {
        let to = self.path(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(self.path(from), to).unwrap();
    }

    fn tick(&mut self) -> u64 {
        self.clock += 60;
        self.clock
    }

    fn dated(&mut self) -> [(&'static str, String); 2] {
        let time = self.tick();
        [
            ("GIT_AUTHOR_DATE", format!("{time} +0000")),
            ("GIT_COMMITTER_DATE", format!("{time} +0000")),
        ]
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"])
    }

    /// Stages everything and commits it, returning the new commit id.
    pub fn commit(&mut self, message: &str) -> String {
        self.run(&["add", "--all"]);
        self.commit_staged(message, &[])
    }

    /// Commits the index as it is, without picking up the working directory.
    pub fn commit_index(&mut self, message: &str) -> String {
        self.commit_staged(message, &[])
    }

    pub fn commit_empty(&mut self, message: &str) -> String {
        self.commit_staged(message, &["--allow-empty"])
    }

    /// Commits everything with an author who is not the committer, in other time zones.
    pub fn commit_as(
        &mut self,
        message: &str,
        author: (&str, &str),
        committer: (&str, &str),
    ) -> String {
        self.run(&["add", "--all"]);
        let author_time = self.tick();
        let committer_time = self.tick() + 3600;
        let envs = [
            ("GIT_AUTHOR_NAME", author.0.to_string()),
            ("GIT_AUTHOR_EMAIL", author.1.to_string()),
            ("GIT_AUTHOR_DATE", format!("{author_time} +0200")),
            ("GIT_COMMITTER_NAME", committer.0.to_string()),
            ("GIT_COMMITTER_EMAIL", committer.1.to_string()),
            ("GIT_COMMITTER_DATE", format!("{committer_time} -0500")),
        ];
        self.run_with(&["commit", "--quiet", "--allow-empty", "-m", message], &envs);
        self.head()
    }

    fn commit_staged(&mut self, message: &str, extra: &[&str]) -> String {
        let envs = self.dated();
        let mut args = vec!["commit", "--quiet", "-m", message];
        args.extend_from_slice(extra);
        self.run_with(&args, &envs);
        self.head()
    }

    /// Merges with a merge commit even when a fast-forward would do.
    pub fn merge(&mut self, branches: &[&str], message: &str) -> String {
        self.merge_with(&[], branches, message)
    }

    pub fn merge_with(&mut self, strategy: &[&str], branches: &[&str], message: &str) -> String {
        let envs = self.dated();
        let mut args = vec!["merge", "--quiet", "--no-ff", "-m", message];
        args.extend_from_slice(strategy);
        args.extend_from_slice(branches);
        self.run_with(&args, &envs);
        self.head()
    }

    pub fn checkout_new(&self, branch: &str) {
        self.run(&["checkout", "--quiet", "-b", branch]);
    }

    pub fn checkout_new_from(&self, branch: &str, start: &str) {
        self.run(&["checkout", "--quiet", "-b", branch, start]);
    }

    pub fn checkout(&self, branch: &str) {
        self.run(&["checkout", "--quiet", branch]);
    }

    pub fn checkout_orphan(&self, branch: &str) {
        self.run(&["checkout", "--quiet", "--orphan", branch]);
        self.run(&["rm", "-rf", "--quiet", "."]);
    }

    /// Records a mode, such as an executable bit, without touching the content.
    pub fn chmod(&self, relative: &str, executable: bool) {
        let flag = if executable {
            "--chmod=+x"
        } else {
            "--chmod=-x"
        };
        self.run(&["update-index", flag, relative]);
    }

    /// Adds a symbolic link to the index directly, which works whatever the platform.
    pub fn symlink(&self, relative: &str, target: &str) {
        let blob = self.hash_object(target.as_bytes());
        self.run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{blob},{relative}"),
        ]);
    }

    /// Adds a submodule entry to the index without needing the submodule to exist.
    pub fn submodule(&self, relative: &str, commit: &str) {
        self.run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},{relative}"),
        ]);
    }

    fn hash_object(&self, content: &[u8]) -> String {
        let path = self.path(".scenario-blob.tmp");
        std::fs::write(&path, content).unwrap();
        let hash = self.run(&["hash-object", "-w", ".scenario-blob.tmp"]);
        std::fs::remove_file(path).unwrap();
        hash
    }

    pub fn tag(&self, name: &str) {
        self.run(&["tag", name]);
    }
}

/// Deterministic pseudo random bytes, so that binary content is reproducible.
pub fn noise(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

