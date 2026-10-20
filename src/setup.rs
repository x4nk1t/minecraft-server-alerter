//! `mcsa setup`: an interactive wizard, or `--non-interactive` for scripts.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::Args;
use inquire::validator::Validation;
use inquire::{Confirm, CustomUserError, Select, Text};

use crate::config::{
    Config, format_duration, is_discord_webhook, parse_address, parse_duration, pretty_path,
    validate_webhook_url,
};
use crate::ui::{self, Pending};
use crate::{discord, monitor, service};

#[derive(Args)]
pub struct SetupArgs {
    /// Don't ask questions; take everything from flags (and the existing config, if any)
    #[arg(long, visible_alias = "no-interactive")]
    pub non_interactive: bool,
    /// Server address, e.g. play.example.com or 1.2.3.4:25566
    #[arg(long, value_name = "ADDRESS")]
    pub host: Option<String>,
    /// Discord webhook URL
    #[arg(long, value_name = "URL")]
    pub webhook_url: Option<String>,
    /// Who to ping on DOWN alerts: @everyone, @here, <@&ROLE_ID>, <@USER_ID>, or none
    #[arg(long)]
    pub mention: Option<String>,
    /// Name shown in Discord messages [default: the host]
    #[arg(long)]
    pub name: Option<String>,
    /// How often to check, e.g. 10m
    #[arg(long, value_name = "DURATION")]
    pub interval: Option<String>,
    /// Only save the config; don't install the background service
    #[arg(long)]
    pub no_service: bool,
    /// Don't ping the server or look up the webhook before saving
    #[arg(long, requires = "non_interactive")]
    pub skip_checks: bool,
}

const MIN_INTERVAL: Duration = Duration::from_secs(30);
const PING_TIMEOUT: Duration = Duration::from_secs(5);

pub fn setup(args: &SetupArgs, config_path: &Path, custom_path: bool) -> Result<()> {
    let existing = match Config::load_optional(config_path) {
        Ok(existing) => existing,
        Err(e) => {
            ui::warn(format!("Ignoring your current config: {e:#}"));
            None
        }
    };
    if args.non_interactive {
        non_interactive(args, config_path, custom_path, existing)
    } else {
        ensure!(
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
            "setup needs a terminal to ask questions. For scripts, use \
             `mcsa setup --non-interactive --host <ADDRESS> --webhook-url <URL>`"
        );
        interactive(args, config_path, custom_path, existing)
    }
}

fn interactive(
    args: &SetupArgs,
    config_path: &Path,
    custom_path: bool,
    existing: Option<Config>,
) -> Result<()> {
    println!();
    println!("  {}", ui::bold("Minecraft Server Alerter setup"));
    println!(
        "  {}",
        ui::dim("Get a Discord message when your Minecraft server goes down, and when it's back.")
    );
    if existing.is_some() {
        println!(
            "  {}",
            ui::dim("Your current settings are filled in. Press Enter to keep them.")
        );
    } else {
        println!(
            "  {}",
            ui::dim("Press Esc to cancel. Nothing is saved until the end.")
        );
    }
    println!();

    let default_address = args.host.clone().or(existing.as_ref().map(Config::address));
    let (host, port) = ask_server(default_address.as_deref())?;

    let default_name = args
        .name
        .clone()
        .or(existing.as_ref().and_then(|c| c.name.clone()))
        .unwrap_or_else(|| host.clone());
    let name = Text::new("Name to show in Discord:")
        .with_default(&default_name)
        .with_help_message("Used in alerts, e.g. \"My SMP is DOWN\"")
        .prompt()?;
    let name = Some(name.trim().to_owned()).filter(|n| !n.is_empty() && *n != host);
    let display_name = name.clone().unwrap_or_else(|| host.clone());

    let default_url = args
        .webhook_url
        .clone()
        .or(existing.as_ref().map(|c| c.webhook_url.clone()));
    let webhook_url = ask_webhook(default_url.as_deref())?;
    if Confirm::new("Send a test message to that channel now?")
        .with_default(existing.is_none())
        .prompt()?
    {
        let sent = {
            let _p = Pending::start("Sending…");
            discord::send(&webhook_url, &discord::test(&display_name))
        };
        match sent {
            Ok(()) => ui::ok("Sent. Check your Discord channel"),
            Err(e) => ui::warn(format!("Couldn't send it: {e:#}")),
        }
    }

    let default_mention = args
        .mention
        .clone()
        .or(existing.as_ref().and_then(|c| c.mention.clone()));
    let mention = ask_mention(default_mention.as_deref())?;

    let default_interval = match &args.interval {
        Some(i) => i.clone(),
        None => format_duration(
            existing
                .as_ref()
                .map_or(Duration::from_secs(600), |c| c.interval),
        ),
    };
    let interval = Text::new("How often should it check?")
        .with_default(&default_interval)
        .with_help_message("e.g. 5m, 10m or 1h (at least 30s)")
        .with_validator(validator(|s| {
            ensure!(
                parse_duration(s)? >= MIN_INTERVAL,
                "check at most every 30s"
            );
            Ok(())
        }))
        .prompt()?;

    let want_service = if args.no_service {
        false
    } else {
        match service::available() {
            Ok(()) => Confirm::new("Run it in the background and start it automatically on boot?")
                .with_default(true)
                .with_help_message("Installs a systemd user service. No root needed")
                .prompt()?,
            Err(e) => {
                ui::warn(format!("Can't set up the background service here: {e:#}"));
                false
            }
        }
    };

    let mut cfg = existing.unwrap_or_else(|| Config::new(host.clone(), port, webhook_url.clone()));
    cfg.host = host;
    cfg.port = port;
    cfg.name = name;
    cfg.webhook_url = webhook_url;
    cfg.mention = mention;
    cfg.interval = parse_duration(&interval)?;
    cfg.validate()?;
    finish(&cfg, config_path, custom_path, want_service)
}

fn ask_server(default: Option<&str>) -> Result<(String, Option<u16>)> {
    let mut retry_with = String::new();
    loop {
        let mut prompt = Text::new("Server address:")
            .with_help_message(
                "What players type into Minecraft, e.g. play.example.com or 1.2.3.4:25566",
            )
            .with_validator(validator(|s| parse_address(s).map(drop)));
        prompt = match default {
            _ if !retry_with.is_empty() => prompt.with_initial_value(&retry_with),
            Some(d) => prompt.with_default(d),
            None => prompt.with_placeholder("play.example.com"),
        };
        let input = prompt.prompt()?.trim().to_owned();
        let (host, port) = parse_address(&input)?;

        let result = {
            let _p = Pending::start(format!("Pinging {input}…"));
            monitor::ping_once(&host, port, PING_TIMEOUT)
        };
        match result {
            Ok(s) => {
                ui::ok(format!("{input} is online: {}", monitor::describe(&s)));
                return Ok((host, port));
            }
            Err(e) => {
                ui::fail(format!("Couldn't reach it: {e:#}"));
                let choice = Select::new(
                    "What now?",
                    vec![
                        "Fix the address",
                        "Use it anyway (the server is just down right now)",
                    ],
                )
                .raw_prompt()?;
                if choice.index == 1 {
                    return Ok((host, port));
                }
                retry_with = input;
            }
        }
    }
}

fn ask_webhook(default: Option<&str>) -> Result<String> {
    let mut retry_with = String::new();
    loop {
        let mut prompt = Text::new("Discord webhook URL:")
            .with_help_message("In Discord: Server Settings → Integrations → Webhooks → New Webhook → Copy Webhook URL")
            .with_validator(validator(validate_webhook_url));
        prompt = match default {
            _ if !retry_with.is_empty() => prompt.with_initial_value(&retry_with),
            Some(d) => prompt.with_default(d),
            None => prompt.with_placeholder("https://discord.com/api/webhooks/…"),
        };
        let url = prompt.prompt()?.trim().to_owned();
        if !is_discord_webhook(&url) {
            ui::warn("That doesn't look like a Discord webhook URL, but let's try it");
        }

        let result = {
            let _p = Pending::start("Checking the webhook…");
            discord::webhook_name(&url)
        };
        match result {
            Ok(name) => {
                ui::ok(format!("Found the webhook “{name}”"));
                return Ok(url);
            }
            Err(e) => {
                ui::fail(format!("{e:#}"));
                let choice = Select::new("What now?", vec!["Enter the URL again", "Use it anyway"])
                    .raw_prompt()?;
                if choice.index == 1 {
                    return Ok(url);
                }
                retry_with = url;
            }
        }
    }
}

fn ask_mention(current: Option<&str>) -> Result<Option<String>> {
    let options = vec![
        "Nobody, just post the message",
        "@everyone",
        "@here (only people who are online)",
        "A role",
        "A specific person",
    ];
    let cursor = match current {
        None | Some("none") => 0,
        Some("@everyone") => 1,
        Some("@here") => 2,
        Some(m) if m.starts_with("<@&") => 3,
        Some(_) => 4,
    };
    let choice = Select::new("Who should get pinged when it goes down?", options)
        .with_starting_cursor(cursor)
        .raw_prompt()?;
    let current_id = current
        .map(|m| {
            m.trim_start_matches("<@")
                .trim_start_matches(['&', '!'])
                .trim_end_matches('>')
        })
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    Ok(match choice.index {
        0 => None,
        1 => Some("@everyone".into()),
        2 => Some("@here".into()),
        3 => Some(format!(
            "<@&{}>",
            ask_id(
                "Role ID:",
                "Turn on Developer Mode (User Settings → Advanced), then Server Settings → Roles → ⋯ → Copy Role ID",
                current_id.filter(|_| cursor == 3),
            )?
        )),
        _ => Some(format!(
            "<@{}>",
            ask_id(
                "User ID:",
                "Turn on Developer Mode (User Settings → Advanced), then right-click the person → Copy User ID",
                current_id.filter(|_| cursor == 4),
            )?
        )),
    })
}

fn ask_id(message: &str, help: &str, default: Option<&str>) -> Result<String> {
    let clean = |s: &str| {
        s.trim()
            .trim_start_matches("<@")
            .trim_start_matches(['&', '!'])
            .trim_end_matches('>')
            .to_owned()
    };
    let mut prompt = Text::new(message)
        .with_help_message(help)
        .with_validator(validator(move |s| {
            let id = clean(s);
            ensure!(
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
                "IDs are long numbers, like 123456789012345678"
            );
            Ok(())
        }));
    if let Some(d) = default {
        prompt = prompt.with_default(d);
    }
    Ok(clean(&prompt.prompt()?))
}

fn non_interactive(
    args: &SetupArgs,
    config_path: &Path,
    custom_path: bool,
    existing: Option<Config>,
) -> Result<()> {
    let mut cfg = match existing {
        Some(cfg) => cfg,
        None => {
            let host = args
                .host
                .as_deref()
                .context("--host is required for a first-time setup")?;
            let url = args
                .webhook_url
                .clone()
                .context("--webhook-url is required for a first-time setup")?;
            let (host, port) = parse_address(host)?;
            Config::new(host, port, url)
        }
    };
    for (key, value) in [
        ("host", &args.host),
        ("webhook_url", &args.webhook_url),
        ("mention", &args.mention),
        ("name", &args.name),
        ("interval", &args.interval),
    ] {
        if let Some(value) = value {
            cfg.set(key, value)
                .with_context(|| format!("--{}", key.replace('_', "-")))?;
        }
    }

    if !args.skip_checks {
        match monitor::ping_once(&cfg.host, cfg.port, PING_TIMEOUT) {
            Ok(s) => ui::ok(format!(
                "{} is online: {}",
                cfg.address(),
                monitor::describe(&s)
            )),
            Err(e) => ui::warn(format!(
                "Couldn't reach {} right now ({e:#}), saving anyway",
                cfg.address()
            )),
        }
        let name = discord::webhook_name(&cfg.webhook_url)
            .context("checking the Discord webhook (pass --skip-checks to save anyway)")?;
        ui::ok(format!("Found the webhook “{name}”"));
    }
    if !args.no_service {
        service::available().context("pass --no-service to only save the config")?;
    }
    finish(&cfg, config_path, custom_path, !args.no_service)
}

fn finish(cfg: &Config, config_path: &Path, custom_path: bool, want_service: bool) -> Result<()> {
    println!();
    cfg.save(config_path)?;
    ui::ok(format!(
        "Saved your settings to {}",
        pretty_path(config_path)
    ));

    if want_service {
        let bin = service::install_binary()?;
        ui::ok(format!("Installed mcsa to {}", pretty_path(&bin)));
        if !service::linger_enabled() {
            match service::enable_linger() {
                Ok(()) => ui::ok(
                    "Turned on lingering, so it keeps running after you log out and starts on boot",
                ),
                Err(e) => ui::warn(format!(
                    "Couldn't turn on lingering, so it will stop when you log out. {e:#}"
                )),
            }
        }
        service::install(&bin, custom_path.then_some(config_path))?;
        ui::ok("Started the background service");
    }

    println!();
    if want_service {
        println!(
            "  {} mcsa is checking {} every {}.",
            ui::bold("All set!"),
            ui::cyan(cfg.address()),
            format_duration(cfg.interval)
        );
        if !service::bin_dir_on_path() {
            ui::warn(
                "~/.local/bin isn't on your PATH yet. Log out and back in so the `mcsa` command works",
            );
        }
    } else {
        println!(
            "  {} Run {} to start checking in this terminal.",
            ui::bold("Saved."),
            ui::cyan("mcsa run")
        );
    }
    println!();
    let mut next_steps = vec![("mcsa status", "Is the server up? Is the alerter running?")];
    if want_service {
        next_steps.push(("mcsa logs -f", "Watch the checks as they happen"));
        next_steps.push((
            "mcsa config",
            "See or change settings, then apply them with `mcsa reload`",
        ));
    } else {
        next_steps.push(("mcsa config", "See or change settings"));
        next_steps.push((
            "mcsa setup",
            "Run again any time to install the background service",
        ));
    }
    for (cmd, what) in next_steps {
        println!("  {}  {}", ui::cyan(format!("{cmd:<13}")), ui::dim(what));
    }
    println!();
    Ok(())
}

/// Adapts an anyhow check into an inquire validator that shows the error inline.
fn validator(
    check: impl Fn(&str) -> Result<()> + Clone + 'static,
) -> impl Fn(&str) -> Result<Validation, CustomUserError> + Clone + 'static {
    move |input: &str| {
        Ok(match check(input) {
            Ok(()) => Validation::Valid,
            Err(e) => Validation::Invalid(format!("{e:#}").into()),
        })
    }
}
