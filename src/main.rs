mod commands;
mod config;
mod discord;
mod monitor;
mod ping;
mod service;
mod setup;
mod srv;
mod ui;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::builder::PossibleValuesParser;
use clap::{Parser, Subcommand};
use inquire::InquireError;

use config::Config;

/// Minecraft Server Alerter: get a Discord message when your Minecraft server goes down.
#[derive(Parser)]
#[command(
    name = "mcsa",
    version,
    arg_required_else_help = true,
    after_help = "New here? Run `mcsa setup`."
)]
struct Cli {
    /// Use this config file instead of ~/.config/minecraft-server-alerter/config.toml
    #[arg(long, global = true, env = "MCSA_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set everything up: pick the server, connect Discord, start the background service
    Setup(setup::SetupArgs),
    /// Is the server up? Is the alerter running?
    Status,
    /// Show or change settings
    Config {
        #[command(subcommand)]
        action: Option<ConfigCmd>,
    },
    /// Apply config changes by restarting the background service
    Reload,
    /// Show the background service's log
    Logs {
        /// Keep watching for new lines
        #[arg(short, long)]
        follow: bool,
        /// How many past lines to show
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: u32,
    },
    /// Send a test message to Discord
    TestWebhook,
    /// Check forever in this terminal (this is what the background service runs)
    Run,
    /// Check once, alert if the server changed state, then exit
    Check,
    /// Stop and remove the background service and the mcsa command
    Uninstall {
        /// Also delete your settings
        #[arg(long)]
        purge: bool,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show all settings (the default)
    Show,
    /// Print one setting's value
    Get {
        #[arg(value_parser = PossibleValuesParser::new(config::KEYS))]
        key: String,
    },
    /// Change a setting, e.g. `mcsa config set interval 5m`
    Set {
        #[arg(value_parser = PossibleValuesParser::new(config::KEYS))]
        key: String,
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        value: Vec<String>,
    },
    /// Reset a setting to its default
    Unset {
        #[arg(value_parser = PossibleValuesParser::new(config::KEYS))]
        key: String,
    },
    /// Open the config file in your editor
    Edit,
    /// Print where the config file is
    Path,
}

fn main() -> ExitCode {
    restore_sigpipe();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if is_cancel(&e) => {
            println!("  Cancelled. Nothing was changed.");
            ExitCode::from(130)
        }
        Err(e) => {
            eprintln!("{} {e:#}", ui::red("error:"));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let custom_path = cli.config.is_some();
    let path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    match cli.command {
        Cmd::Setup(args) => {
            refuse_sudo()?;
            setup::setup(&args, &path, custom_path)
        }
        Cmd::Status => commands::status(&path),
        Cmd::Config { action } => match action.unwrap_or(ConfigCmd::Show) {
            ConfigCmd::Show => commands::config_show(&path),
            ConfigCmd::Get { key } => commands::config_get(&path, &key),
            ConfigCmd::Set { key, value } => commands::config_set(&path, &key, &value.join(" ")),
            ConfigCmd::Unset { key } => commands::config_unset(&path, &key),
            ConfigCmd::Edit => commands::config_edit(&path),
            ConfigCmd::Path => {
                println!("{}", path.display());
                Ok(())
            }
        },
        Cmd::Reload => {
            refuse_sudo()?;
            commands::reload(&path)
        }
        Cmd::Logs { follow, lines } => commands::logs(follow, lines),
        Cmd::TestWebhook => commands::test_webhook(&path),
        Cmd::Run => monitor::run(&Config::load(&path)?, &config::state_path(&path)),
        Cmd::Check => monitor::check(&Config::load(&path)?, &config::state_path(&path)),
        Cmd::Uninstall { purge, yes } => {
            refuse_sudo()?;
            commands::uninstall(&path, purge, yes)
        }
    }
}

/// mcsa installs into the current user's home and systemd, so under sudo it would set
/// itself up for root instead of you.
fn refuse_sudo() -> Result<()> {
    if env::var_os("SUDO_USER").is_some() && env::var("USER").as_deref() == Ok("root") {
        bail!("run this without sudo. mcsa sets itself up for your own user and doesn't need root");
    }
    Ok(())
}

/// Rust ignores SIGPIPE, which turns `mcsa config | head` into a panic. Restore the
/// default so piping into tools that stop reading early just ends quietly.
fn restore_sigpipe() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    // SAFETY: called once at startup before any threads exist.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

fn is_cancel(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<InquireError>(),
            Some(InquireError::OperationCanceled | InquireError::OperationInterrupted)
        )
    })
}
