use std::path::Path;
use lore::interface::{LoreEvent, LoreGlobalArgs, LoreString};
use crate::source;

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
async fn create_lore_repository(destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if destination.join(".lore").exists() {
        return Err(format!("{} already contains a Lore repository, refusing to import into it", destination.display()).into());
    }

    std::fs::create_dir_all(destination)?;

    let globals = LoreGlobalArgs {
        repository_path: LoreString::from(destination.to_string_lossy().as_ref()),
        offline: 1,
        ..Default::default()
    };

    let arguments = lore::repository::LoreRepositoryCreateArgs {
        repository_url: LoreString::default(),
        description: LoreString::default(),
        id: LoreString::default(),
        vfs: lore::repository::LoreVfsType::None,
        use_shared_store: lore::interface::LoreSharedStoreMode::Inherit,
        shared_store_path: LoreString::default(),
    };

    let status = lore::repository::create(globals, arguments, Some(Box::new(report_error) as Box<_>)).await;
    if status != 0 {
        return Err(format!("Failed to create Lore repository in '{}' (status {status})", destination.display()).into());
    }

    Ok(())
}

pub async fn migrate_to(repository: &gix::Repository, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let branches = source::get_branches(repository)?;
    create_lore_repository(destination).await?;

    Ok(())
}
