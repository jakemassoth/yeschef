use anyhow::Result;

use crate::config::{check_host_deps, check_platform, resolve_home};
use crate::store::Store;

/// Run `nixsand init`.
pub fn run_init() -> Result<()> {
    // Platform check first (fail fast)
    check_platform()?;

    let home = resolve_home()?;
    let was_new = !home.exists();

    // Create home dir and projects subdir
    std::fs::create_dir_all(home.join("projects"))
        .map_err(|e| anyhow::anyhow!("failed to create nixsand home directory: {e}"))?;

    // Initialize the database (idempotent schema migration)
    let db_path = home.join("nixsand.db");
    let _store = Store::open(&db_path)?;

    if was_new {
        println!("nixsand home initialized at {}", home.display());
    } else {
        println!("nixsand home already exists at {} (no changes)", home.display());
    }

    // Check host dependencies
    check_host_deps()?;
    println!("all host dependencies verified (container, tmux, git)");

    Ok(())
}
