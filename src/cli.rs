//! The command line: what `ax` understands and what each command runs. It
//! lives in the library so a program built on ax can offer the same commands.

use anyhow::Result;
use std::path::PathBuf;
use usage::{Args, Cli, Subcommands};

use crate::{account, auto_switch, completions, session};

/// Multi-account switcher for Claude Code
#[derive(Cli)]
#[usage(bin = "ax", version, unknown_flags = "error", completion)]
pub struct Cli {
    #[usage(subcommand)]
    pub command: Command,
}

#[derive(Subcommands)]
pub enum Command {
    /// Manage stored accounts
    Account {
        #[usage(subcommand)]
        command: AccountCommand,
    },
    /// Switch the default Claude Code login to an account
    Switch {
        /// Account number, email, or alias
        account: String,
    },
    /// Watch usage and switch before hitting a rate limit
    AutoSwitch(AutoSwitchArgs),
    /// Run Claude Code as a specific account in this terminal only
    Run(RunArgs),
    /// Map a directory to an account for `ax run`
    Map {
        directory: PathBuf,
        /// Account number, email, or alias
        #[usage(long)]
        to: String,
    },
    /// Manage directory mappings
    Mapping {
        #[usage(subcommand)]
        command: MappingCommand,
    },
    /// Print or install the shell completion script
    ShellCompletion {
        #[usage(subcommand)]
        command: ShellCompletionCommand,
    },
}

#[derive(Subcommands)]
pub enum AccountCommand {
    /// Store the currently logged-in account, or one from a setup token
    Add {
        /// Register a long-lived setup token instead of the current login
        #[usage(long)]
        token: Option<String>,
        /// Label for a token-registered account
        #[usage(long, requires = "--token")]
        email: Option<String>,
        /// Short alias for the account
        #[usage(long)]
        alias: Option<String>,
    },
    /// List stored accounts
    List,
    /// Give an account a short alias
    Alias {
        /// Account number, email, or alias
        account: String,
        alias: String,
    },
    /// Remove a stored account
    Remove {
        /// Account number, email, or alias
        account: String,
    },
}

#[derive(Args)]
pub struct AutoSwitchArgs {
    /// Switch when the active account's usage reaches this percentage
    #[usage(long, default = "90.0")]
    pub threshold: f64,
    /// Seconds between usage checks
    #[usage(long, default = "60")]
    pub interval: u64,
    /// Check once and exit instead of looping
    #[usage(long)]
    pub once: bool,
}

#[derive(Args)]
pub struct RunArgs {
    /// Account number, email, or alias; defaults to the mapping for the current directory
    #[usage(long)]
    pub account: Option<String>,
    /// Arguments forwarded to claude
    #[usage(double_dash = "required")]
    pub claude_args: Vec<String>,
}

#[derive(Subcommands)]
pub enum MappingCommand {
    /// List directory mappings
    List,
    /// Remove a directory mapping
    Remove { directory: PathBuf },
}

#[derive(Subcommands)]
pub enum ShellCompletionCommand {
    /// Write the completion script to stdout
    Print {
        /// The shell to generate for
        #[usage(
            choices("bash", "elvish", "zsh", "fish", "nu", "powershell"),
            choices_strict = false
        )]
        shell: String,
    },
    /// Write the completion script where the shell looks for it
    Install {
        /// The shell to install for
        #[usage(
            choices("bash", "elvish", "zsh", "fish", "nu", "powershell"),
            choices_strict = false
        )]
        shell: String,
    },
}

pub fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Account { command } => match command {
            AccountCommand::Add {
                token,
                email,
                alias,
            } => account::add(token, email, alias),
            AccountCommand::List => account::list(),
            AccountCommand::Alias { account, alias } => account::set_alias(&account, &alias),
            AccountCommand::Remove { account } => account::remove(&account),
        },
        Command::Switch { account } => account::switch(&account),
        Command::AutoSwitch(args) => auto_switch::run(args.threshold, args.interval, args.once),
        Command::Run(args) => session::run(args.account.as_deref(), &args.claude_args),
        Command::Map { directory, to } => account::map(&directory, &to),
        Command::Mapping { command } => match command {
            MappingCommand::List => account::list_mappings(),
            MappingCommand::Remove { directory } => account::unmap(&directory),
        },
        Command::ShellCompletion { command } => match command {
            ShellCompletionCommand::Print { shell } => completions::print(&shell),
            ShellCompletionCommand::Install { shell } => completions::install(&shell),
        },
    }
}
