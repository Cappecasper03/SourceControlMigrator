//! Writes the scenario repository used by the tests, so it can be migrated by hand:
//! `cargo run --example make_test_repo -- ./target/scenarios`
#[path = "../tests/common/mod.rs"]
mod common;

fn main() {
    let directory = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "./target/scenarios".to_string()),
    );
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    common::build_scenario_repository(&directory);
    println!("{}", directory.display());
}
