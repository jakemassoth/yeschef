mod backend;
mod cli;
mod commands;
mod config;
mod guard;
mod names;
mod store;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Commands, ProjectCommands, TicketCommands};
use commands::{cleanup, orchestrate, project, tui};
use config::Config;

fn main() {
    let cli = Cli::parse();

    // Set up logging based on verbosity
    match cli.verbose {
        0 => {} // no extra logging
        1 => eprintln!("[yeschef] verbose mode enabled"),
        _ => eprintln!("[yeschef] trace mode enabled"),
    }

    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init => return commands::init::run_init(),
        Commands::Project(project_args) => {
            let config = Config::load()?;
            match project_args.command {
                ProjectCommands::Add { git_url, name } => {
                    project::run_add(&config, &git_url, name.as_deref())?;
                }
                ProjectCommands::List => project::run_list(&config)?,
            }
            return Ok(());
        }
        Commands::Refresh { project } => {
            let config = Config::load()?;
            project::run_refresh(&config, project.as_deref())?;
            return Ok(());
        }
        Commands::Ticket(ticket_args) => {
            let config = Config::load()?;
            match ticket_args.command {
                TicketCommands::StatusSet { status } => orchestrate::run_ticket_status_set(
                    &config,
                    &ticket_args.project,
                    &ticket_args.branch,
                    status,
                )?,
            }
            return Ok(());
        }
        _ => {}
    }

    // Orchestration commands all need a loaded config.
    let config = Config::load()?;
    match cli.command {
        Commands::Spawn {
            project,
            branch,
            base,
            agent,
            prompt,
        } => orchestrate::run_spawn(
            &config,
            &project,
            &branch,
            base.as_deref(),
            &agent,
            prompt.as_deref(),
        )?,
        Commands::Send {
            project,
            branch,
            text,
        } => orchestrate::run_send(&config, &project, &branch, &text.join(" "))?,
        Commands::Peek {
            project,
            branch,
            lines,
        } => orchestrate::run_peek(&config, &project, &branch, lines)?,
        Commands::Status => orchestrate::run_status(&config)?,
        Commands::Tui => tui::run_tui(&config)?,
        Commands::Attach { project, branch } => {
            orchestrate::run_attach(&config, project.as_deref(), branch.as_deref())?;
        }
        Commands::Kill {
            project,
            branch,
            rm_worktree,
        } => orchestrate::run_kill(&config, &project, &branch, rm_worktree)?,
        Commands::Cleanup { project, yes } => {
            cleanup::run_cleanup(&config, project.as_deref(), yes)?;
        }
        Commands::Init | Commands::Project(_) | Commands::Refresh { .. } | Commands::Ticket(_) => {
            unreachable!("handled above")
        }
    }
    Ok(())
}
