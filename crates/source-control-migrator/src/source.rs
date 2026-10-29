use std::collections::{BTreeSet, HashMap, HashSet};

fn hash_path(path: &str) -> String {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(path.as_bytes());

    let id = hasher.try_finalize().unwrap();
    id.to_hex_with_len(16).to_string()
}

fn get_cache_path(path: &str) -> std::path::PathBuf {
    std::env::temp_dir().join("SourceControlMigrator").join(hash_path(path))
}

fn get_progress_root() -> (std::sync::Arc<prodash::tree::Root>, prodash::render::line::JoinHandle,
) {
    let root = prodash::tree::Root::new();
    let renderer = prodash::render::line(
        std::io::stderr(),
        std::sync::Arc::downgrade(&root),
        prodash::render::line::Options {
            frames_per_second: 1.0,
            throughput: true,
            ..Default::default()
        }.auto_configure(prodash::render::line::StreamKind::Stderr),
    );
    (root, renderer)
}

fn fetch(repository: &gix::Repository) -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching into '{}'", repository.git_dir().display());

    let (root, renderer) = get_progress_root();

    let remote = repository.find_fetch_remote(None)?;
    let refspecs = remote.with_refspecs(["+refs/heads/*:refs/heads/*"], gix::remote::Direction::Fetch)?;
    let connect = refspecs.connect(gix::remote::Direction::Fetch)?;
    let prepare = connect.prepare_fetch(root.add_child("prepare"), Default::default())?;
    prepare.receive(root.add_child("fetch"), &gix::interrupt::IS_INTERRUPTED)?;

    drop(renderer);
    Ok(())
}

fn clone(source: &str, destination: &std::path::Path) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    if destination.exists() {
        println!("Removing existing cache: '{}'", destination.display());
        std::fs::remove_dir_all(destination)?;
    }

    println!("Cloning '{source}' into '{}'", destination.display());
    std::fs::create_dir_all(destination)?;

    let (root, renderer) = get_progress_root();

    let mut prepare = gix::prepare_clone_bare(source, destination)?;
    let result = prepare.fetch_only(root.add_child("clone"), &gix::interrupt::IS_INTERRUPTED);

    drop(renderer);

    match result {
        Ok((repo, _outcome)) => Ok(repo),
        Err(error) => Err(error.into())
    }
}

fn fetch_or_clone(source: &str, destination: &std::path::Path) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    match gix::open(destination) {
        Ok(repository) => {
            if let Err(error) = fetch(&repository) {
                println!("Fetch failed ({error}), recloning");
            } else {
                return Ok(repository);
            }
        }
        Err(error) => println!("Repository can't be opened ({error}), recloning"),
    }

    clone(source, destination)
}

pub fn open(source: &str) -> Result<gix::Repository, Box<dyn std::error::Error>> {
    let cache_path = get_cache_path(source);
    fetch_or_clone(source, &cache_path)
}

pub fn get_commits(repository: &gix::Repository) -> Result<Vec<(gix::ObjectId, String)>, Box<dyn std::error::Error>> {
    let mut branch_tips = Vec::new();
    for reference in repository.references()?.all()?.filter_map(Result::ok) {
        let branch_name = reference.name().to_string();
        if let Ok(id) = reference.into_fully_peeled_id() {
            branch_tips.push((branch_name, id.detach()));
        }
    }

    let mut tips = Vec::new();
    let mut branches: Vec<(String, HashSet<gix::ObjectId>)> = Vec::new();
    for (branch_name, tip_id) in branch_tips {
        tips.push(tip_id);
        if let Ok(walk) = repository.rev_walk([tip_id]).all() {
            branches.push((branch_name, walk.filter_map(Result::ok).map(|info| info.id).collect()));
        }
    }

    if branches.is_empty() {
        return Err("The source repository doesn't have branches".into());
    }

    let head_name = repository.head_name()?.unwrap().to_string();
    branches.sort_by(|(a_name, a_commits), (b_name, b_commits)| {
        let is_main = |name: &str| name == head_name;
        is_main(b_name).cmp(&is_main(a_name)).then(a_commits.len().cmp(&b_commits.len())).then_with(|| a_name.cmp(b_name))
    });

    let mut commit_to_branch: HashMap<gix::ObjectId, &str> = HashMap::new();
    for (name, commits) in &branches {
        for commit in commits {
            commit_to_branch.entry(*commit).or_insert(name.as_str());
        }
    }

    let mut revisions: Vec<_> = repository.rev_walk(tips).all()?.filter_map(Result::ok).collect();
    revisions.reverse();

    let index_of: HashMap<gix::ObjectId, usize> = revisions.iter().enumerate().map(|(index, revision)| (revision.id, index)).collect();

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); revisions.len()];
    let mut pending_parents = vec![0usize; revisions.len()];
    for (index, revision) in revisions.iter().enumerate() {
        for parent in &revision.parent_ids {
            if let Some(&parent_index) = index_of.get(parent) {
                children[parent_index].push(index);
                pending_parents[index] += 1;
            }
        }
    }

    let mut ready: BTreeSet<usize> = (0..revisions.len()).filter(|&index| pending_parents[index] == 0).collect();
    let mut commits = Vec::with_capacity(revisions.len());
    let mut current_branch: Option<&str> = None;

    while !ready.is_empty() {
        let next = ready.iter().copied().find(|&index| Some(commit_to_branch[&revisions[index].id]) == current_branch).or_else(|| ready.first().copied()).unwrap();
        ready.remove(&next);

        let id = revisions[next].id;
        let branch = commit_to_branch[&id];
        current_branch = Some(branch);
        commits.push((id, branch.to_string()));

        for &child in &children[next] {
            pending_parents[child] -= 1;
            if pending_parents[child] == 0 {
                ready.insert(child);
            }
        }
    }

    Ok(commits)
}
