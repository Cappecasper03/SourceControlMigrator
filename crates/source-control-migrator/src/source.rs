use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

fn hash_path(path: &str) -> String {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(path.as_bytes());

    let id = hasher.try_finalize().unwrap();
    id.to_hex_with_len(16).to_string()
}

fn get_cache_path(path: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("SourceControlMigrator")
        .join(hash_path(path))
}

fn get_progress_root() -> (
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

fn fetch(repository: &gix::Repository) -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching into '{}'", repository.git_dir().display());

    let (root, renderer) = get_progress_root();

    let remote = repository.find_fetch_remote(None)?;
    let refspecs = remote.with_refspecs(
        ["+refs/heads/*:refs/heads/*"],
        gix::remote::Direction::Fetch,
    )?;
    let connect = refspecs.connect(gix::remote::Direction::Fetch)?;
    let prepare = connect.prepare_fetch(root.add_child("prepare"), Default::default())?;
    prepare.receive(root.add_child("fetch"), &gix::interrupt::IS_INTERRUPTED)?;

    drop(renderer);
    Ok(())
}

fn clone(
    source: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
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
        Err(error) => Err(error.into()),
    }
}

fn fetch_or_clone(
    source: &str,
    destination: &std::path::Path,
) -> Result<gix::Repository, Box<dyn std::error::Error>> {
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

// TODO: Refactor below
struct Tip {
    name: String,
    index: u32,
    reach: u32,
}

fn pop_live(heap: &mut BinaryHeap<Reverse<u32>>, done: &[bool]) -> Option<u32> {
    while let Some(Reverse(commit)) = heap.pop() {
        if !done[commit as usize] {
            return Some(commit);
        }
    }
    None
}

pub fn get_commits(
    repository: &gix::Repository,
) -> Result<Vec<(gix::ObjectId, String)>, Box<dyn std::error::Error>> {
    let mut reference_tips = Vec::new();
    for reference in repository.references()?.all()?.filter_map(Result::ok) {
        let branch_name = reference.name().to_string();
        if let Ok(id) = reference.into_fully_peeled_id() {
            reference_tips.push((branch_name, id.detach()));
        }
    }

    if reference_tips.is_empty() {
        return Err("The source repository doesn't have branches".into());
    }

    let walk_tips: Vec<_> = reference_tips.iter().map(|(_, id)| *id).collect();
    let mut revisions: Vec<_> = repository
        .rev_walk(walk_tips)
        .all()?
        .filter_map(Result::ok)
        .collect();
    revisions.reverse();
    let count = revisions.len();

    let ids: Vec<_> = revisions.iter().map(|revision| revision.id).collect();
    let index_of: HashMap<_, _> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index as u32))
        .collect();

    let mut parent_start = Vec::with_capacity(count + 1);
    let mut parent_edges = Vec::new();
    parent_start.push(0);

    for revision in &revisions {
        for parent in &revision.parent_ids {
            if let Some(&parent_index) = index_of.get(parent) {
                parent_edges.push(parent_index);
            }
        }

        parent_start.push(parent_edges.len() as u32);
    }

    let mut tips: Vec<_> = reference_tips
        .into_iter()
        .filter_map(|(name, id)| {
            index_of.get(&id).map(|&index| Tip {
                name,
                index,
                reach: 0,
            })
        })
        .collect();

    let get_parents = |commit: usize| {
        &parent_edges[parent_start[commit] as usize..parent_start[commit + 1] as usize]
    };

    let mut child_start = vec![0u32; count + 1];
    for &parent_index in &parent_edges {
        child_start[parent_index as usize + 1] += 1;
    }

    for index in 0..count {
        child_start[index + 1] += child_start[index];
    }

    let mut child_start_clone = child_start.clone();
    let mut child_edges = vec![0u32; parent_edges.len()];
    for commit_index in 0..count {
        for &parent_index in get_parents(commit_index) {
            child_edges[child_start_clone[parent_index as usize] as usize] = commit_index as u32;
            child_start_clone[parent_index as usize] += 1;
        }
    }

    let get_children = |commit: usize| {
        &child_edges[child_start[commit] as usize..child_start[commit + 1] as usize]
    };

    let pending_template: Vec<_> = (0..count)
        .map(|index| parent_start[index + 1] - parent_start[index])
        .collect();

    let mut topology = Vec::with_capacity(count);
    let mut pending_template_clone = pending_template.clone();
    let mut stack: Vec<_> = (0..count as u32)
        .filter(|&index| pending_template_clone[index as usize] == 0)
        .collect();
    while let Some(commit_index) = stack.pop() {
        topology.push(commit_index);

        for &child_index in get_children(commit_index as usize) {
            pending_template_clone[child_index as usize] -= 1;
            if pending_template_clone[child_index as usize] == 0 {
                stack.push(child_index);
            }
        }
    }

    let mut mask = vec![0u64; count];
    for chunk_start in (0..tips.len()).step_by(64) {
        let chunk_end = (chunk_start + 64).min(tips.len());
        mask.fill(0);

        for tip in chunk_start..chunk_end {
            mask[tips[tip].index as usize] |= 1u64 << (tip - chunk_start);
        }

        for &commit_index in topology.iter().rev() {
            let bits = mask[commit_index as usize];
            if bits != 0 {
                for &parent_index in get_parents(commit_index as usize) {
                    mask[parent_index as usize] |= bits;
                }
            }
        }

        for &bits in &mask {
            let mut bits = bits;
            while bits != 0 {
                tips[chunk_start + bits.trailing_zeros() as usize].reach += 1;
                bits &= bits - 1;
            }
        }
    }

    let head_name = repository.head_name()?.map(|name| name.to_string());
    let is_main = |name: &str| head_name.as_deref() == Some(name);
    tips.sort_by(|a, b| {
        is_main(&b.name)
            .cmp(&is_main(&a.name))
            .then(a.reach.cmp(&b.reach))
            .then_with(|| a.name.cmp(&b.name))
    });

    const NONE: u32 = u32::MAX;
    let mut branch_of = vec![NONE; count];
    let mut stack = Vec::new();
    for (priority, tip) in tips.iter().enumerate() {
        if branch_of[tip.index as usize] != NONE {
            continue;
        }

        branch_of[tip.index as usize] = priority as u32;
        stack.push(tip.index);

        while let Some(commit_index) = stack.pop() {
            for &parent_index in get_parents(commit_index as usize) {
                if branch_of[parent_index as usize] == NONE {
                    branch_of[parent_index as usize] = priority as u32;
                    stack.push(parent_index);
                }
            }
        }
    }

    let mut pending = pending_template;
    let mut done = vec![false; count];
    let mut global = BinaryHeap::new();
    let mut per_branch: Vec<_> = (0..tips.len()).map(|_| BinaryHeap::new()).collect();
    for commit_index in 0..count as u32 {
        if pending[commit_index as usize] == 0 {
            global.push(Reverse(commit_index));
            per_branch[branch_of[commit_index as usize] as usize].push(Reverse(commit_index));
        }
    }

    let mut commits = Vec::with_capacity(count);
    let mut current_index: Option<usize> = None;
    loop {
        let next_index = current_index
            .and_then(|branch_index| pop_live(&mut per_branch[branch_index], &done))
            .or_else(|| pop_live(&mut global, &done));
        let Some(next_index) = next_index else { break };
        done[next_index as usize] = true;

        let branch_index = branch_of[next_index as usize] as usize;
        current_index = Some(branch_index);
        commits.push((ids[next_index as usize], tips[branch_index].name.clone()));

        for &child_index in get_children(next_index as usize) {
            pending[child_index as usize] -= 1;
            if pending[child_index as usize] == 0 {
                global.push(Reverse(child_index));
                per_branch[branch_of[child_index as usize] as usize].push(Reverse(child_index));
            }
        }
    }

    Ok(commits)
}
