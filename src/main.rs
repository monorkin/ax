mod account;
mod auto_switch;
mod claude;
mod completions;
mod fsutil;
mod locks;
mod mappings;
mod oauth;
mod paths;
mod session;
mod store;

use anyhow::Result;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use usage::{Args, Cli, Subcommands};

/// Multi-account switcher for Claude Code
#[derive(Cli)]
#[usage(bin = "ax", version, unknown_flags = "error", completion)]
struct Cli {
    #[usage(subcommand)]
    command: Command,
}

#[derive(Subcommands)]
enum Command {
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
enum AccountCommand {
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
struct AutoSwitchArgs {
    /// Switch when the active account's usage reaches this percentage
    #[usage(long, default = "90.0")]
    threshold: f64,
    /// Seconds between usage checks
    #[usage(long, default = "60")]
    interval: u64,
    /// Check once and exit instead of looping
    #[usage(long)]
    once: bool,
}

#[derive(Args)]
struct RunArgs {
    /// Account number, email, or alias; defaults to the mapping for the current directory
    #[usage(long)]
    account: Option<String>,
    /// Arguments forwarded to claude
    #[usage(double_dash = "required")]
    claude_args: Vec<String>,
}

#[derive(Subcommands)]
enum MappingCommand {
    /// List directory mappings
    List,
    /// Remove a directory mapping
    Remove { directory: PathBuf },
}

#[derive(Subcommands)]
enum ShellCompletionCommand {
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

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
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

pub fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs();
    format_epoch_seconds(seconds)
}

fn format_epoch_seconds(seconds: u64) -> String {
    let days_since_epoch = seconds / 86_400;
    let (year, month, day) = civil_date(days_since_epoch);
    let hour = seconds / 3600 % 24;
    let minute = seconds / 60 % 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date(days_since_epoch: u64) -> (u64, u64, u64) {
    let days = days_since_epoch + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}
