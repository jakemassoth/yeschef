use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "yeschef",
    about = "Orchestrate coding agents across git worktrees via zmx"
)]
pub struct Cli {
    /// Increase verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize the yeschef home directory and validate dependencies
    Init,

    /// Manage projects (add / list)
    Project(ProjectArgs),

    /// Fetch the latest remote refs into a project's bare clone
    Refresh {
        /// Project to refresh (omit to refresh all registered projects)
        project: Option<String>,
    },

    /// Create a worktree and launch an agent in a zmx session
    Spawn {
        /// Project name
        project: String,
        /// Branch name (created from base if it doesn't exist)
        branch: String,
        /// Base branch or commit (defaults to the project's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Agent command to launch in the window
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Initial prompt passed as the agent's trailing argument
        #[arg(short, long)]
        prompt: Option<String>,
    },

    /// Send a one-line instruction to a running agent
    Send {
        /// Project name
        project: String,
        /// Branch name
        branch: String,
        /// The instruction (remaining args are joined with spaces)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        text: Vec<String>,
    },

    /// Print the recent output of an agent's pane
    Peek {
        /// Project name
        project: String,
        /// Branch name
        branch: String,
        /// Number of pane lines to show
        #[arg(short = 'n', long)]
        lines: Option<usize>,
    },

    /// List all tickets and whether their agents are still running
    Status,

    /// Open an interactive TUI to watch the brigade's live output
    Tui,

    /// Attach to a yeschef zmx session to watch the brigade
    Attach {
        /// Optional project to select a specific ticket window
        project: Option<String>,
        /// Optional branch to select a specific ticket window
        branch: Option<String>,
    },

    /// Stop an agent's window (optionally removing its worktree)
    Kill {
        /// Project name
        project: String,
        /// Branch name
        branch: String,
        /// Also remove the git worktree from disk
        #[arg(long)]
        rm_worktree: bool,
    },
}

#[derive(Args, Debug)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommands,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCommands {
    /// Add a new project by git URL
    Add {
        /// Git URL to clone
        git_url: String,
        /// Optional project name (defaults to repo basename)
        name: Option<String>,
    },

    /// List all registered projects
    List,
}
