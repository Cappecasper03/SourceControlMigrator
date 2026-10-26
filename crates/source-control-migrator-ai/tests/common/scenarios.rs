//! One function per history shape the migrator has to cope with. Each works on its own files and
//! branches on top of the base commit, so they can be applied alone or all together.

use std::path::Path;

use super::{Git, noise};

/// The commit every scenario builds on.
pub fn base(git: &mut Git) {
    git.write("README.md", "# Scenarios\n");
    git.commit("Initial commit");
}

pub fn file_kinds(git: &mut Git) {
    git.write("empty.txt", "");
    git.write("no-newline.txt", "last line has no newline");
    git.write("windows.txt", "one\r\ntwo\r\n");
    git.write("binary.bin", noise(4096, 1));
    git.write("big.bin", noise(600 * 1024, 2));
    git.write("with space/file name.txt", "spaces everywhere\n");
    git.write("unicode/ünï-文件-🚀.txt", "unicode path\n");
    git.write("deep/a/b/c/d/e/f/leaf.txt", "deep\n");
    for index in 0..120 {
        git.write(
            &format!("many/file-{index:03}.txt"),
            format!("file {index}\n"),
        );
    }
    git.commit("Many kinds of files");
}

pub fn edit_and_delete(git: &mut Git) {
    git.write("edit/text.txt", "one\n");
    git.write("edit/large.bin", noise(600 * 1024, 3));
    git.write("edit/gone.txt", "gone\n");
    git.commit("Add files to edit");
    git.write("edit/text.txt", "one\ntwo\n");
    let mut bytes = noise(600 * 1024, 3);
    bytes[300_000] ^= 0xff;
    git.write("edit/large.bin", bytes);
    git.commit("Edit a text file and one byte of a large file");
    git.remove("edit/gone.txt");
    git.commit("Delete a file");
}

pub fn rename_and_move(git: &mut Git) {
    git.write("rename/old name.txt", "content\n");
    git.write("rename/dir/a/b/leaf.txt", "leaf\n");
    git.commit("Add files to move");
    git.rename("rename/old name.txt", "rename/new name.txt");
    git.commit("Rename a file");
    git.rename("rename/dir", "rename/moved/deeper");
    git.commit("Move a directory");
}

pub fn directory_removal(git: &mut Git) {
    git.write("removal/a/b/c/leaf.txt", "leaf\n");
    git.write("removal/keep.txt", "keep\n");
    git.commit("Add nested files");
    git.remove("removal/a");
    git.commit("Delete a directory so its parents can go as well");
}

pub fn revert_to_earlier_tree(git: &mut Git) {
    git.write("revert/temp.txt", "temporary\n");
    git.commit("Add a file");
    git.remove("revert/temp.txt");
    git.commit("Remove it again");
    git.write("revert/temp.txt", "temporary\n");
    git.commit("Add the same file again, the tree is an earlier tree");
}

pub fn empty_commits(git: &mut Git) {
    git.commit_empty("An empty commit\n\nWith a body that spans\nseveral lines.\n\n\n");
    git.commit_empty("Another empty commit right after the first");
}

pub fn mode_only_changes(git: &mut Git) {
    git.write("mode/script.sh", "#!/bin/sh\necho hi\n");
    git.commit("Add a script");
    git.chmod("mode/script.sh", true);
    git.commit_index("Only the mode changes");
    git.chmod("mode/script.sh", false);
    git.commit_index("The mode changes back");
}

pub fn file_becomes_directory(git: &mut Git) {
    git.write("swap", "a file\n");
    git.commit("Add a file");
    git.remove("swap");
    git.write("swap/inside.txt", "now a directory\n");
    git.commit("A file turns into a directory");
    git.remove("swap");
    git.write("swap", "a file again\n");
    git.commit("A directory turns into a file");
}

pub fn case_only_rename(git: &mut Git) {
    git.write("Case.txt", "case\n");
    git.commit("Add Case.txt");
    git.run(&["mv", "Case.txt", "case.tmp"]);
    git.run(&["mv", "case.tmp", "case.txt"]);
    git.commit_index("Rename Case.txt to case.txt");
}

pub fn symbolic_link(git: &mut Git) {
    git.symlink("link-to-readme", "README.md");
    git.commit_index("Add a symbolic link");
}

pub fn submodule(git: &mut Git) {
    git.submodule("vendor/library", "1111111111111111111111111111111111111111");
    git.commit_index("Add a submodule");
}

pub fn author_differs_from_committer(git: &mut Git) {
    git.write("authors.txt", "two people\n");
    git.commit_as(
        "Different author and committer",
        ("Bob Builder", "bob@example.com"),
        ("Carol Committer", "carol@example.com"),
    );
}

pub fn unicode_and_multiline_messages(git: &mut Git) {
    git.write("message.txt", "message\n");
    git.commit("Ünïcode subject ✓\n\nBody with 'quotes', \"double quotes\" and a `backtick`.");
    git.write("message.txt", "message\nmore\n");
    git.commit("Subject only");
}

pub fn merge_with_new_content(git: &mut Git) {
    git.checkout_new("feature");
    git.write("feature/one.txt", "one\n");
    git.commit("Feature one");
    git.write("feature/two.txt", "two\n");
    git.commit("Feature two");
    git.checkout("master");
    git.write("master-only.txt", "master moved on\n");
    git.commit("Master moves on");
    git.merge(&["feature"], "Merge branch 'feature'");
}

pub fn merge_equal_to_second_parent(git: &mut Git) {
    git.checkout_new("side-a");
    git.write("side-a.txt", "a\n");
    git.commit("Side A");
    git.checkout("master");
    git.merge(
        &["side-a"],
        "Merge branch 'side-a' without anything new on master",
    );
}

pub fn merge_equal_to_first_parent(git: &mut Git) {
    git.checkout_new("side-b");
    git.write("side-b.txt", "b\n");
    git.commit("Side B");
    git.checkout("master");
    git.write("master-after-b.txt", "x\n");
    git.commit("Master moves on");
    git.merge_with(
        &["-s", "ours"],
        &["side-b"],
        "Merge branch 'side-b' keeping our tree",
    );
}

pub fn octopus_merge(git: &mut Git) {
    git.checkout_new("side-c");
    git.write("side-c.txt", "c\n");
    git.commit("Side C");
    git.checkout("master");
    git.checkout_new("side-d");
    git.write("side-d.txt", "d\n");
    git.commit("Side D");
    git.checkout("master");
    git.merge(&["side-c", "side-d"], "Octopus merge of side-c and side-d");
}

pub fn unmerged_branch(git: &mut Git) {
    git.checkout_new("unmerged");
    git.write("unmerged.txt", "never merged\n");
    git.commit("Unmerged one");
    git.write("unmerged.txt", "never merged, edited\n");
    git.commit("Unmerged two");
}

/// A branch that shares its name with the branch Lore starts on.
pub fn branch_named_main(git: &mut Git) {
    git.checkout_new("main");
    git.write("main-branch.txt", "a source branch named main\n");
    git.commit("A commit on a branch called main");
}

/// A history that shares nothing with the rest.
pub fn unrelated_root(git: &mut Git) {
    git.checkout_orphan("orphan");
    git.write("orphan.txt", "unrelated root\n");
    git.commit("Unrelated root commit");
    git.write("orphan.txt", "unrelated root, edited\n");
    git.commit("Unrelated second commit");
}

/// A tree that becomes empty, followed by commits that stay empty.
pub fn emptied_tree(git: &mut Git) {
    git.checkout_new("emptied");
    git.run(&["rm", "-r", "--quiet", "."]);
    git.commit_empty("Delete every file");
    git.commit_empty("Empty commit on an empty tree");
    git.write("revived.txt", "back again\n");
    git.commit("Add a file to the empty tree");
}

pub type Scenario = fn(&mut Git);

/// Every scenario by name. Applied one after the other they make one repository.
pub const SCENARIOS: &[(&str, Scenario)] = &[
    ("file_kinds", file_kinds),
    ("edit_and_delete", edit_and_delete),
    ("rename_and_move", rename_and_move),
    ("directory_removal", directory_removal),
    ("revert_to_earlier_tree", revert_to_earlier_tree),
    ("empty_commits", empty_commits),
    ("mode_only_changes", mode_only_changes),
    ("file_becomes_directory", file_becomes_directory),
    ("case_only_rename", case_only_rename),
    ("symbolic_link", symbolic_link),
    ("submodule", submodule),
    (
        "author_differs_from_committer",
        author_differs_from_committer,
    ),
    (
        "unicode_and_multiline_messages",
        unicode_and_multiline_messages,
    ),
    ("merge_with_new_content", merge_with_new_content),
    (
        "merge_equal_to_second_parent",
        merge_equal_to_second_parent,
    ),
    ("merge_equal_to_first_parent", merge_equal_to_first_parent),
    ("octopus_merge", octopus_merge),
    ("unmerged_branch", unmerged_branch),
    ("branch_named_main", branch_named_main),
    ("unrelated_root", unrelated_root),
    ("emptied_tree", emptied_tree),
];

/// Creates a repository holding one scenario on top of the base commit.
pub fn build_scenario(dir: &Path, scenario: Scenario) -> Git {
    let mut git = Git::init(dir);
    base(&mut git);
    scenario(&mut git);
    git.checkout("master");
    git
}

/// Creates a repository with every scenario in it, checked out on `master`.
pub fn build_scenario_repository(dir: &Path) -> Git {
    let mut git = Git::init(dir);
    base(&mut git);
    for (_, scenario) in SCENARIOS {
        scenario(&mut git);
        git.checkout("master");
    }
    git.tag("v1");
    git
}
