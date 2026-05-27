mod backend;
mod cli;
mod commands;
mod config;
mod guard;
mod image;
mod names;
mod store;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Commands, ProjectCommands};
use config::Config;

fn main() {
    let cli = Cli::parse();

    // Set up logging based on verbosity
    match cli.verbose {
        0 => {} // no extra logging
        1 => eprintln!("[nixsand] verbose mode enabled"),
        _ => eprintln!("[nixsand] trace mode enabled"),
    }

    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init => {
            commands::init::run_init()?;
        }
        Commands::Project(project_args) => {
            // Most project commands need a full config
            match project_args.command {
                ProjectCommands::Add { git_url, name } => {
                    let config = Config::load()?;
                    commands::project::run_add(&config, &git_url, name.as_deref())?;
                }
                ProjectCommands::List => {
                    let config = Config::load()?;
                    commands::project::run_list(&config)?;
                }
                ProjectCommands::Branch {
                    project,
                    branch,
                    base,
                } => {
                    let config = Config::load()?;
                    commands::project::run_branch(&config, &project, &branch, base.as_deref())?;
                }
                ProjectCommands::Attach { project, branch } => {
                    let config = Config::load()?;
                    commands::project::run_attach(&config, &project, &branch)?;
                }
            }
        }
    }
    Ok(())
}
