//! The command line: what `ax` understands and what each command runs. It
//! lives in the library so a program built on ax can offer the same commands.

use anyhow::{Result, bail};
use std::path::PathBuf;
use usage::{Args, Cli, Subcommands};

use crate::store::Provider;
use crate::{account, auto_switch, completions, pool, session};

/// Multi-account switcher for Claude Code and Codex
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
    /// Switch the default login of the account's tool to it
    Switch {
        /// Account number, email, or alias
        account: String,
    },
    /// Watch usage and switch before hitting a rate limit
    AutoSwitch(AutoSwitchArgs),
    /// Run Claude Code or Codex as an account or pool in this terminal only
    Run(RunArgs),
    /// Group accounts that rotate among themselves
    Pool {
        #[usage(subcommand)]
        command: PoolCommand,
    },
    /// Map a directory to an account for `ax run`, one per tool
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
    /// Store a tool's current login, or a Claude account from a setup token
    Add {
        /// Which tool's login to store
        #[usage(choices("claude", "codex"))]
        tool: String,
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

#[derive(Subcommands)]
pub enum PoolCommand {
    /// Add accounts to a pool, creating it if it's new
    Add {
        pool: String,
        /// Account numbers, emails, or aliases
        accounts: Vec<String>,
    },
    /// List pools and their members, marking who each pool is logged in as
    List,
    /// Take accounts out of a pool, or remove the pool if none are given
    Remove {
        pool: String,
        /// Account numbers, emails, or aliases
        accounts: Vec<String>,
    },
}

#[derive(Args)]
pub struct AutoSwitchArgs {
    /// Rotate this pool's own login among its members, instead of the default logins
    #[usage(long)]
    pub pool: Option<String>,
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
    /// Which tool to run; defaults to the account's if one is given, otherwise claude
    #[usage(choices("claude", "codex"))]
    pub tool: Option<String>,
    /// Account number, email, or alias; defaults to the tool's mapping for the current directory
    #[usage(long)]
    pub account: Option<String>,
    /// Run on this pool's own login, which `auto-switch --pool` rotates among its members
    #[usage(long)]
    pub pool: Option<String>,
    /// Arguments forwarded to claude or codex
    #[usage(double_dash = "required")]
    pub claude_args: Vec<String>,
}

#[derive(Subcommands)]
pub enum MappingCommand {
    /// List directory mappings
    List,
    /// Remove a directory's mappings, or only one tool's
    Remove {
        directory: PathBuf,
        /// Which tool's mapping to remove; both if not given
        #[usage(choices("claude", "codex"))]
        tool: Option<String>,
    },
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

fn provider_named(tool: &str) -> Result<Provider> {
    match tool {
        "claude" => Ok(Provider::Claude),
        "codex" => Ok(Provider::Codex),
        _ => bail!("unknown tool '{tool}' — expected claude or codex"),
    }
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Account { command } => match command {
            AccountCommand::Add {
                tool,
                token,
                email,
                alias,
            } => match provider_named(&tool)? {
                Provider::Claude => account::add(token, email, alias),
                Provider::Codex if token.is_some() => {
                    bail!("--token is for Claude Code accounts; Codex accounts come from the current login")
                }
                Provider::Codex => account::add_codex(alias),
            },
            AccountCommand::List => account::list(),
            AccountCommand::Alias { account, alias } => account::set_alias(&account, &alias),
            AccountCommand::Remove { account } => account::remove(&account),
        },
        Command::Switch { account } => account::switch(&account),
        Command::AutoSwitch(args) => auto_switch::watch(args.threshold, args.interval, args.once, args.pool.as_deref()),
        Command::Run(args) => {
            let tool = args.tool.as_deref().map(provider_named).transpose()?;
            session::launch(args.account.as_deref(), args.pool.as_deref(), tool, &args.claude_args)
        }
        Command::Pool { command } => match command {
            PoolCommand::Add { pool, accounts } => pool::add(&pool, &accounts),
            PoolCommand::List => pool::list(),
            PoolCommand::Remove { pool, accounts } => pool::remove(&pool, &accounts),
        },
        Command::Map { directory, to } => account::map(&directory, &to),
        Command::Mapping { command } => match command {
            MappingCommand::List => account::list_mappings(),
            MappingCommand::Remove { directory, tool } => {
                let tool = tool.as_deref().map(provider_named).transpose()?;
                account::unmap(&directory, tool)
            }
        },
        Command::ShellCompletion { command } => match command {
            ShellCompletionCommand::Print { shell } => completions::print(&shell),
            ShellCompletionCommand::Install { shell } => completions::install(&shell),
        },
    }
}
