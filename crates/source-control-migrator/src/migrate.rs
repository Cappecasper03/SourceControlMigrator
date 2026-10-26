use std::path::Path;
use crate::source;

pub async fn to(repository: &gix::Repository, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let branches = source::get_branches(repository)?;

    Ok(())
}
