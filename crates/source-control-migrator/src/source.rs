use gix::traverse::commit::topo::{Builder, Sorting};

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

fn get_branch_tips(repository: &gix::Repository) -> Result<Vec<(String, gix::ObjectId)>, Box<dyn std::error::Error>> {
    let mut local = std::collections::BTreeMap::new();
    let mut remote = std::collections::BTreeMap::new();

    for reference in repository.references()?.all()? {
        let mut reference = reference?;
        let name = reference.name().as_bstr().to_string();

        let (branches, short) = if let Some(short) = name.strip_prefix("refs/heads/") {
            (&mut local, short)
        } else if let Some(short) = name.strip_prefix("refs/remotes/") {
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

pub fn get_commits(repository: &gix::Repository) -> Result<Vec<(gix::ObjectId, String)>, Box<dyn std::error::Error>> {
    let branch_tips = get_branch_tips(repository)?;
    if branch_tips.is_empty() {
        return Err("The source repository doesn't have branches".into());
    }

    let mut commits_with_time = Vec::new();
    let mut seen_tips = Vec::new();

    for (name, tip) in branch_tips {
        let builder = Builder::new(&repository.objects);
        let builder = builder.with_tips([tip]);
        let builder = builder.with_ends(seen_tips.iter().copied());
        let builder = builder.sorting(Sorting::DateOrder);
        let builder = builder.build()?;

        for info in builder {
            let id = info?.id;
            let time = repository.find_commit(id)?.time()?.seconds;
            commits_with_time.push((time, id, name.clone()));
        }

        seen_tips.push(tip);
    }

    commits_with_time.sort_by_key(|&(time, _, _)| time);

    let commits = commits_with_time.into_iter().map(|(_, id, name)| (id, name)).collect();
    Ok(commits)
}
