use anyhow::{Context, Result};

use crate::config::{check_host_deps, resolve_home};
use crate::store::Store;

/// The kitchen manual, shipped into the yeschef home so the head chef
/// agent (launched from `~/.yeschef`) loads it automatically.
const AGENTS_MD: &str = include_str!("../../AGENTS.md");

/// Run `yeschef init`.
pub fn run_init() -> Result<()> {
    let home = resolve_home()?;
    let was_new = !home.exists();

    // Create home dir and projects subdir
    std::fs::create_dir_all(home.join("projects"))
        .context("failed to create yeschef home directory")?;

    // Initialize the database (idempotent schema migration)
    let db_path = home.join("yeschef.db");
    let _store = Store::open(&db_path)?;

    // Drop the kitchen manual so the head chef agent finds it.
    std::fs::write(home.join("AGENTS.md"), AGENTS_MD).context("failed to write AGENTS.md")?;

    if was_new {
        println!("yeschef home initialized at {}", home.display());
    } else {
        println!(
            "yeschef home already exists at {} (refreshed AGENTS.md)",
            home.display()
        );
    }

    // Check host dependencies
    check_host_deps()?;
    println!("all host dependencies verified (git, zmx)");
    println!(
        "run your head chef agent from {} to load AGENTS.md",
        home.display()
    );

    Ok(())
}
