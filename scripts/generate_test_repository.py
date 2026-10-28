import os
import shutil
import subprocess
import sys
from pathlib import Path

REPOSITORY_DIR = Path(__file__).resolve().parent.parent / "tests/repository"

def run_git(args, cwd=REPOSITORY_DIR, env=None, check=True):
    full_env = os.environ.copy()
    # Ensure consistent output and no prompts
    full_env["GIT_TERMINAL_PROMPT"] = "0"
    if env:
        full_env.update(env)
    res = subprocess.run(
        ["git"] + args,
        cwd=cwd,
        env=full_env,
        capture_output=True,
        text=True,
        check=False
    )
    if check and res.returncode != 0:
        raise RuntimeError(f"Git command failed: git {' '.join(args)}\nSTDOUT: {res.stdout}\nSTDERR: {res.stderr}")
    return res.stdout.strip()

def hash_object(data: bytes) -> str:
    res = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"],
        cwd=REPOSITORY_DIR,
        input=data,
        capture_output=True,
        check=True
    )
    return res.stdout.strip().decode() if isinstance(res.stdout, bytes) else res.stdout.strip()

def write_file(rel_path: str, content):
    path = REPOSITORY_DIR / rel_path
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, str):
        path.write_text(content, encoding="utf-8", newline="")
    else:
        path.write_bytes(content)

def remove_file(rel_path: str):
    path = REPOSITORY_DIR / rel_path
    if path.is_file() or path.is_symlink():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)

clock_time = 1704067200  # 2024-01-01 00:00:00 UTC

def next_date(offset_minutes=1):
    global clock_time
    clock_time += offset_minutes * 60
    return f"{clock_time} +0000"

def commit(message: str, author=None, committer=None, allow_empty=False, extra_args=None):
    env = {}
    d = next_date()
    env["GIT_AUTHOR_DATE"] = d
    env["GIT_COMMITTER_DATE"] = d

    if author:
        name, email, date_str = author
        env["GIT_AUTHOR_NAME"] = name
        env["GIT_AUTHOR_EMAIL"] = email
        if date_str:
            env["GIT_AUTHOR_DATE"] = date_str
    if committer:
        name, email, date_str = committer
        env["GIT_COMMITTER_NAME"] = name
        env["GIT_COMMITTER_EMAIL"] = email
        if date_str:
            env["GIT_COMMITTER_DATE"] = date_str

    cmd = ["commit", "-m", message]
    if allow_empty:
        cmd.append("--allow-empty")
    if extra_args:
        cmd.extend(extra_args)
    run_git(cmd, env=env)

def main():
    if REPOSITORY_DIR.exists():
        print(f"Cleaning existing test repository directory: {REPOSITORY_DIR}")
        # On Windows, read-only files in .git/objects might need permission fix
        def on_rm_error(func, path, exc_info):
            os.chmod(path, 0o777)
            func(path)
        shutil.rmtree(REPOSITORY_DIR, onerror=on_rm_error)

    REPOSITORY_DIR.mkdir(parents=True, exist_ok=True)
    print(f"Initializing repository at {REPOSITORY_DIR}...")
    run_git(["init", "--initial-branch=main"])
    run_git(["config", "user.name", "Alice Author"])
    run_git(["config", "user.email", "alice@example.com"])
    run_git(["config", "core.autocrlf", "false"])
    run_git(["config", "core.quotepath", "false"])
    run_git(["config", "commit.gpgsign", "false"])

    # 1. Base initial commit
    print("1. Creating base commit...")
    write_file("README.md", "# Test Migration Repository\n\nThis repository is designed to test source control migration tools against all common and edge-case Git features.\n")
    write_file(".gitignore", "*.tmp\n.DS_Store\nThumbs.db\n")
    write_file(".editorconfig", "root = true\n\n[*]\nindent_style = space\nindent_size = 4\nend_of_line = lf\ncharset = utf-8\n")
    run_git(["add", "."])
    commit("Initial commit: repository baseline setup")
    run_git(["tag", "v0.1.0-baseline"])

    # 2. File kinds: empty, CRLF, LF, mixed, no-newline, unicode, binary, nested, deep
    print("2. Adding various file formats and edge cases...")
    write_file("files/empty.txt", "")
    write_file("files/no_newline.txt", "This line intentionally has no newline at EOF")
    write_file("files/crlf.txt", "Line 1\r\nLine 2\r\nLine 3\r\n")
    write_file("files/lf.txt", "Line 1\nLine 2\nLine 3\n")
    write_file("files/mixed_endings.txt", "Line 1\r\nLine 2\nLine 3\r\nLine 4\n")
    write_file("files/unicode_content.txt", "Hello 🌍! Éléphant, München, 北京, 東京, Привет, العربية, 🚀, 💻.\n")
    
    # Path edge cases
    write_file("paths with spaces/nested folder/file with spaces.txt", "Testing path with spaces\n")
    write_file("paths_unicode/ünïcöde-目录/файл-🚀.txt", "Unicode file and directory names\n")
    write_file("paths_special/file-1.0.0_beta(v2)[final]#tag@test.txt", "Filename with tricky punctuation\n")
    write_file("deep/level1/level2/level3/level4/level5/leaf.txt", "Deep directory hierarchy leaf\n")
    
    # Bulk files (50 small files)
    for i in range(1, 51):
        write_file(f"bulk/item_{i:03d}.txt", f"Bulk item #{i}\n")

    # Binary files: small binary containing every byte 0..255 and nulls
    all_bytes = bytes(range(256)) * 4
    write_file("binary/all_bytes.bin", all_bytes)
    # Larger binary file (~200KB deterministic pseudo-random)
    large_bytes = bytearray()
    seed = 0x12345678
    for _ in range(200 * 1024):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        large_bytes.append(seed >> 16 & 0xFF)
    write_file("binary/large_asset.dat", bytes(large_bytes))

    run_git(["add", "."])
    commit("Add diverse file kinds: CRLF, LF, empty, no-newline, binary, unicode paths, deep tree")

    # 3. Executable permission, Symlink, Submodule
    print("3. Adding mode changes, symlink, and submodule git objects...")
    write_file("scripts/run.sh", "#!/usr/bin/env bash\necho 'Hello from script'\n")
    run_git(["add", "scripts/run.sh"])
    run_git(["update-index", "--chmod=+x", "scripts/run.sh"])
    commit("Add script with executable bit set (+x)")

    # Flip mode back and forth
    run_git(["update-index", "--chmod=-x", "scripts/run.sh"])
    commit("Remove executable bit from script (-x)")

    # Symlink created via git update-index (works cross-platform on Windows)
    symlink_blob = hash_object(b"README.md")
    run_git(["update-index", "--add", "--cacheinfo", f"120000,{symlink_blob},symlink-to-readme"])
    commit("Add symbolic link pointing to README.md")

    # Git submodule entry (gitlink 160000)
    run_git(["update-index", "--add", "--cacheinfo", "160000,1111111111111111111111111111111111111111,vendor/submodule_lib"])
    write_file(".gitmodules", '[submodule "vendor/submodule_lib"]\n\tpath = vendor/submodule_lib\n\turl = https://github.com/example/submodule_lib.git\n')
    run_git(["add", ".gitmodules"])
    commit("Add git submodule entry (gitlink mode 160000)")

    # 4. Content edits, renames, moves, and deletions
    print("4. Testing edits, renames, moves, and removals...")
    # Edit text and binary
    write_file("files/lf.txt", "Line 1\nLine 2 - edited\nLine 3\nLine 4 - appended\n")
    large_bytes_mod = bytearray(large_bytes)
    large_bytes_mod[1000] ^= 0xFF
    large_bytes_mod[50000] ^= 0xAA
    write_file("binary/large_asset.dat", bytes(large_bytes_mod))
    run_git(["add", "."])
    commit("Edit text file lines and mutate binary bytes in large asset")

    # Rename file
    write_file("rename_me.txt", "Content to be renamed\n")
    run_git(["add", "rename_me.txt"])
    commit("Add file before rename")
    run_git(["mv", "rename_me.txt", "renamed_file.txt"])
    commit("Rename rename_me.txt to renamed_file.txt")

    # Rename with simultaneous modification
    write_file("renamed_and_modified.txt", "Initial content\n")
    run_git(["add", "renamed_and_modified.txt"])
    commit("Add renamed_and_modified.txt")
    remove_file("renamed_and_modified.txt")
    write_file("moved_and_modified.txt", "Initial content\nAppended new content\n")
    run_git(["add", "--all"])
    commit("Simultaneously move and modify file")

    # Directory move
    write_file("docs/chapter1/intro.md", "# Introduction\n")
    write_file("docs/chapter1/summary.md", "# Summary\n")
    run_git(["add", "docs"])
    commit("Add docs/chapter1 structure")
    run_git(["mv", "docs/chapter1", "docs/getting_started"])
    commit("Move directory from docs/chapter1 to docs/getting_started")

    # Directory complete removal
    write_file("obsolete/a.txt", "A\n")
    write_file("obsolete/b.txt", "B\n")
    write_file("obsolete/nested/c.txt", "C\n")
    run_git(["add", "obsolete"])
    commit("Add obsolete directory structure")
    run_git(["rm", "-r", "obsolete"])
    commit("Completely remove obsolete directory structure")

    # Case-only rename (Windows safe staging via git mv)
    write_file("CaseSensitive.txt", "Testing case change\n")
    run_git(["add", "CaseSensitive.txt"])
    commit("Add CaseSensitive.txt")
    run_git(["mv", "CaseSensitive.txt", "case_temp.txt"])
    run_git(["mv", "case_temp.txt", "casesensitive.txt"])
    commit("Perform case-only rename: CaseSensitive.txt -> casesensitive.txt")

    # File becomes directory & directory becomes file
    write_file("type_flip", "I am a plain file\n")
    run_git(["add", "type_flip"])
    commit("Add type_flip as regular file")
    remove_file("type_flip")
    write_file("type_flip/nested.txt", "Now type_flip is a directory containing this file\n")
    run_git(["add", "--all"])
    commit("Convert type_flip from file into directory")
    remove_file("type_flip")
    write_file("type_flip", "I am back to being a single regular file\n")
    run_git(["add", "--all"])
    commit("Convert type_flip back from directory into file")

    # Revert to an earlier tree state
    write_file("tree_revert.txt", "Version 1\n")
    run_git(["add", "tree_revert.txt"])
    commit("Add tree_revert.txt")
    remove_file("tree_revert.txt")
    run_git(["add", "--all"])
    commit("Delete tree_revert.txt")
    write_file("tree_revert.txt", "Version 1\n")
    run_git(["add", "tree_revert.txt"])
    commit("Recreate tree_revert.txt with exact original content (tree matches earlier tree)")

    # 5. Commit metadata: Author != Committer, multiline, unicode commit message, empty commits
    print("5. Adding commit metadata variations (authors, unicode messages, empty commits)...")
    write_file("metadata.txt", "Testing authors and committers\n")
    run_git(["add", "metadata.txt"])
    commit(
        "Commit with different author and committer\n\nAuthor is Bob in +0200, Committer is Charlie in -0500.",
        author=("Bob Developer", "bob@external.org", "1704153600 +0200"),
        committer=("Charlie ReleaseManager", "charlie@company.com", "1704157200 -0500")
    )

    write_file("metadata.txt", "Testing multiline unicode commit messages\n")
    run_git(["add", "metadata.txt"])
    unicode_msg = (
        "✨ feat(parser): support UTF-8 üñîçødë & 🚀 emojis\n\n"
        "Detailed explanation of changes:\n"
        "- Supported characters: 中文, 日本語, Русский, Español\n"
        "- Escaped characters: `backticks`, \"double quotes\", 'single quotes', <tags>, & ampersand\n"
        "- Math symbols: ∑, ∫, ≈, ≠, ≤, ≥\n\n"
        "Resolves: #1042\n"
        "Signed-off-by: Alice Author <alice@example.com>"
    )
    commit(unicode_msg)

    # Empty commits
    commit("An empty commit with no file changes", allow_empty=True)
    commit("Another empty commit right after\n\nWith multiple blank lines and body.\n", allow_empty=True)

    # 6. Branching & Merging Scenarios
    print("6. Creating branches and merge topologies...")

    # Scenario A: Standard feature branch with 2 commits, merged with --no-ff
    run_git(["checkout", "-b", "feature/standard-merge"])
    write_file("features/feature_a.txt", "Feature A initial\n")
    run_git(["add", "."])
    commit("Feature branch commit 1")
    write_file("features/feature_a.txt", "Feature A initial\nFeature A enhancement\n")
    run_git(["add", "."])
    commit("Feature branch commit 2")

    run_git(["checkout", "main"])
    write_file("main_progress.txt", "Main moved forward while feature was being developed\n")
    run_git(["add", "."])
    commit("Main advance commit")

    run_git(["merge", "--no-ff", "-m", "Merge branch 'feature/standard-merge' into main", "feature/standard-merge"])

    # Scenario B: Merge commit with new content / manual resolution
    run_git(["checkout", "-b", "feature/conflict-branch"])
    write_file("shared_doc.txt", "Line from branch\n")
    run_git(["add", "."])
    commit("Commit on conflict-branch")

    run_git(["checkout", "main"])
    write_file("shared_doc.txt", "Line from main\n")
    run_git(["add", "."])
    commit("Commit on main touching same file")

    # Merge with conflict, resolved manually with custom content
    res = subprocess.run(["git", "merge", "--no-commit", "feature/conflict-branch"], cwd=REPOSITORY_DIR, capture_output=True)
    # Write custom resolved content (not strictly identical to either parent)
    write_file("shared_doc.txt", "Resolved line: synthesized content during merge\nExtra line in merge commit\n")
    run_git(["add", "shared_doc.txt"])
    commit("Merge branch 'feature/conflict-branch' with custom resolved content")

    # Scenario C: Merge equal to second parent
    run_git(["checkout", "-b", "feature/tree-as-second-parent"])
    write_file("replacement.txt", "New replacement tree\n")
    run_git(["add", "."])
    commit("Prepare branch for second-parent merge")
    run_git(["checkout", "main"])
    # Merge using recursive strategy with -X theirs (so result equals feature branch state)
    run_git(["merge", "--no-ff", "-X", "theirs", "-m", "Merge branch 'feature/tree-as-second-parent' adopting second parent changes", "feature/tree-as-second-parent"])

    # Scenario D: Merge equal to first parent (-s ours)
    run_git(["checkout", "-b", "feature/discarded-side"])
    write_file("discarded.txt", "This file should be discarded by ours merge strategy\n")
    run_git(["add", "."])
    commit("Commit on discarded-side")
    run_git(["checkout", "main"])
    write_file("main_after_side.txt", "Main progresses before ours merge\n")
    run_git(["add", "."])
    commit("Main progress before ours merge")
    run_git(["merge", "-s", "ours", "-m", "Merge branch 'feature/discarded-side' keeping our tree (-s ours)", "feature/discarded-side"])

    # Scenario E: Octopus merge (3 parent branches merged at once)
    print("   Creating octopus merge...")
    run_git(["checkout", "-b", "octopus/branch-1", "main"])
    write_file("octopus/one.txt", "Octopus 1\n")
    run_git(["add", "."])
    commit("Octopus branch 1 commit")
    b1_head = run_git(["rev-parse", "HEAD"])

    run_git(["checkout", "-b", "octopus/branch-2", "main"])
    write_file("octopus/two.txt", "Octopus 2\n")
    run_git(["add", "."])
    commit("Octopus branch 2 commit")
    b2_head = run_git(["rev-parse", "HEAD"])

    run_git(["checkout", "-b", "octopus/branch-3", "main"])
    write_file("octopus/three.txt", "Octopus 3\n")
    run_git(["add", "."])
    commit("Octopus branch 3 commit")
    b3_head = run_git(["rev-parse", "HEAD"])

    run_git(["checkout", "main"])
    main_head = run_git(["rev-parse", "HEAD"])
    # Add files from the octopus branches into index/worktree for main
    write_file("octopus/one.txt", "Octopus 1\n")
    write_file("octopus/two.txt", "Octopus 2\n")
    write_file("octopus/three.txt", "Octopus 3\n")
    run_git(["add", "."])
    tree_id = run_git(["write-tree"])
    
    # Create octopus merge commit directly with 4 parents (main + 3 branches)
    # using git commit-tree (C builtin, avoids MSYS sh.exe limitation)
    d = next_date()
    env = {
        "GIT_AUTHOR_DATE": d,
        "GIT_COMMITTER_DATE": d,
        "GIT_AUTHOR_NAME": "Alice Author",
        "GIT_AUTHOR_EMAIL": "alice@example.com",
        "GIT_COMMITTER_NAME": "Alice Author",
        "GIT_COMMITTER_EMAIL": "alice@example.com",
    }
    octopus_commit = run_git([
        "commit-tree", tree_id,
        "-p", main_head,
        "-p", b1_head,
        "-p", b2_head,
        "-p", b3_head,
        "-m", "Octopus merge of branches octopus/branch-1, octopus/branch-2, octopus/branch-3"
    ], env=env)
    run_git(["update-ref", "refs/heads/main", octopus_commit])
    run_git(["reset", "--hard", "main"])

    # Scenario F: Criss-cross merge
    print("   Creating criss-cross merge...")
    run_git(["checkout", "-b", "crisscross/alpha", "main"])
    write_file("crisscross/alpha.txt", "Alpha commit 1\n")
    run_git(["add", "."])
    commit("Crisscross alpha 1")

    run_git(["checkout", "-b", "crisscross/beta", "main"])
    write_file("crisscross/beta.txt", "Beta commit 1\n")
    run_git(["add", "."])
    commit("Crisscross beta 1")

    # Alpha merges beta
    run_git(["checkout", "crisscross/alpha"])
    run_git(["merge", "--no-ff", "-m", "Alpha merges Beta (first cross)", "crisscross/beta"])
    write_file("crisscross/alpha.txt", "Alpha commit 2\n")
    run_git(["add", "."])
    commit("Crisscross alpha 2")

    # Beta merges alpha
    run_git(["checkout", "crisscross/beta"])
    write_file("crisscross/beta.txt", "Beta commit 2\n")
    run_git(["add", "."])
    commit("Crisscross beta 2")
    run_git(["merge", "--no-ff", "-m", "Beta merges Alpha (second cross)", "crisscross/alpha"])

    # Merge into main
    run_git(["checkout", "main"])
    run_git(["merge", "--no-ff", "-m", "Merge crisscross/beta into main", "crisscross/beta"])

    # Scenario G: Unmerged branch (long-lived / pending feature)
    print("   Creating unmerged branches...")
    run_git(["checkout", "-b", "feature/unmerged-work", "main"])
    write_file("unmerged/work_in_progress.txt", "Draft WIP 1\n")
    run_git(["add", "."])
    commit("Unmerged branch: initial work")
    write_file("unmerged/work_in_progress.txt", "Draft WIP 1\nWIP 2\n")
    run_git(["add", "."])
    commit("Unmerged branch: additional work in progress")

    # Scenario H: Complex branch name (slashes, hyphens, dots)
    run_git(["checkout", "-b", "release/v1.2.0-hotfix.3", "main"])
    write_file("release_notes_v1.2.0.txt", "Release hotfix notes\n")
    run_git(["add", "."])
    commit("Hotfix commit on release/v1.2.0-hotfix.3")

    # Scenario I: Branch named 'master' (often coexisting with main)
    run_git(["checkout", "-b", "master", "main"])
    write_file("master_sync.txt", "Legacy master branch tracking main\n")
    run_git(["add", "."])
    commit("Commit on master branch")

    # Scenario J: Unrelated root / Orphan branch
    print("   Creating orphan branch with unrelated root history...")
    run_git(["checkout", "--orphan", "orphan-history"])
    # Clear index and working directory
    run_git(["rm", "-rf", "."])
    write_file("orphan_root.txt", "This root has no common ancestors with main.\n")
    write_file("independent_project/app.py", "print('Orphan app')\n")
    run_git(["add", "."])
    commit("Orphan history initial commit (unrelated root)")
    write_file("independent_project/app.py", "print('Orphan app v2')\n")
    run_git(["add", "."])
    commit("Orphan history second commit")

    # Merge orphan branch into a separate integration branch to test --allow-unrelated-histories
    run_git(["checkout", "-b", "test/merged-orphan", "main"])
    run_git(["merge", "--allow-unrelated-histories", "--no-ff", "-m", "Merge unrelated orphan-history into test/merged-orphan", "orphan-history"])

    # Scenario K: Emptied tree branch
    print("   Creating branch that empties all files, commits on empty tree, and revives...")
    run_git(["checkout", "-b", "test/emptied-tree", "main"])
    run_git(["rm", "-rf", "."])
    commit("Delete every single file in the tree")
    commit("Empty commit on an empty tree", allow_empty=True)
    write_file("revived.txt", "The tree is alive again\n")
    run_git(["add", "."])
    commit("Add revived file back to empty tree")

    # 7. Tags: Lightweight, Annotated, and Tag on non-HEAD commit
    print("7. Creating tags (lightweight, annotated with multiline message)...")
    run_git(["checkout", "main"])
    # Annotated tag with tagger, date, message
    d = next_date()
    env = {
        "GIT_COMMITTER_NAME": "Alice TagAdmin",
        "GIT_COMMITTER_EMAIL": "alice@admin.org",
        "GIT_COMMITTER_DATE": d,
    }
    annotated_msg = "Release version 1.0.0\n\nFeatures:\n- All core file formats\n- Merges and branch topologies\n- Edge cases verified\n"
    run_git(["tag", "-a", "v1.0.0", "-m", annotated_msg], env=env)

    # Lightweight tag on main HEAD
    run_git(["tag", "v1.0.0-latest"])

    # Tag on a specific previous commit
    root_commit = run_git(["rev-list", "--max-parents=0", "HEAD"]).splitlines()[-1]
    run_git(["tag", "-a", "v0.0.1-genesis", "-m", "Genesis tag on the very first commit", root_commit], env=env)

    # Ensure main is checked out
    run_git(["checkout", "main"])
    print("\n[SUCCESS] Successfully generated test repository at:")
    print(f"   {REPOSITORY_DIR}")
    print("\nSummary of branches:")
    branches = run_git(["branch", "-a"])
    print(branches)
    print("\nSummary of tags:")
    tags = run_git(["tag", "-l"])
    print(tags)
    print("\nCommit count on main:")
    commit_count = run_git(["rev-list", "--count", "main"])
    print(f"   {commit_count} commits")

if __name__ == "__main__":
    if sys.platform == "win32":
        try:
            sys.stdout.reconfigure(encoding="utf-8")
        except Exception:
            pass
    main()
