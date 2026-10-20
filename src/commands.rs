//! The smaller commands: config, status, reload, logs, test-webhook, uninstall.

use std::fs;
use std::io::{ErrorKind, IsTerminal, stdin};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use inquire::Confirm;

use crate::config::{
    self, Config, describe_mention, format_duration, mask_webhook_url, pretty_path,
};
use crate::discord::human_duration;
use crate::ui::{self, Pending};
use crate::{discord, monitor, service};

pub fn config_show(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    let or_default =
        |value: Option<String>, default: String| value.unwrap_or_else(|| ui::dim(default));
    println!();
    println!("  {}", ui::dim(pretty_path(path)));
    println!();
    ui::table(&[
        ("host", cfg.host.clone()),
        (
            "port",
            or_default(
                cfg.port.map(|p| p.to_string()),
                "auto (SRV record, else 25565)".into(),
            ),
        ),
        (
            "name",
            or_default(cfg.name.clone(), format!("{} (same as host)", cfg.host)),
        ),
        ("webhook_url", mask_webhook_url(&cfg.webhook_url)),
        ("mention", or_default(cfg.mention.clone(), "nobody".into())),
        ("interval", format_duration(cfg.interval)),
        ("attempts", cfg.attempts.to_string()),
        ("retry_delay", format_duration(cfg.retry_delay)),
        ("timeout", format_duration(cfg.timeout)),
    ]);
    println!();
    println!(
        "  Change a setting with {}, then apply it with {}.",
        ui::cyan("mcsa config set <key> <value>"),
        ui::cyan("mcsa reload")
    );
    println!();
    Ok(())
}

pub fn config_get(path: &Path, key: &str) -> Result<()> {
    println!("{}", Config::load(path)?.get(key)?);
    Ok(())
}

pub fn config_set(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut cfg = Config::load(path)?;
    cfg.set(key, value)?;
    cfg.save(path)?;
    let shown = match key {
        "webhook_url" => mask_webhook_url(&cfg.webhook_url),
        "host" => cfg.address(),
        _ => cfg.get(key)?,
    };
    ui::ok(format!("{key} is now {}", ui::bold(shown)));
    reload_hint();
    Ok(())
}

pub fn config_unset(path: &Path, key: &str) -> Result<()> {
    let mut cfg = Config::load(path)?;
    cfg.unset(key)?;
    cfg.save(path)?;
    let now = match cfg.get(key)? {
        v if v.is_empty() => "unset".to_owned(),
        v => format!("back to the default ({v})"),
    };
    ui::ok(format!("{key} is {now}"));
    reload_hint();
    Ok(())
}

/// Opens the config in $VISUAL/$EDITOR, and only saves it if it's still valid.
pub fn config_edit(path: &Path) -> Result<()> {
    let original = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            bail!(
                "there's no config at {} yet. Run `mcsa setup` first",
                pretty_path(path)
            )
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", pretty_path(path))),
    };
    let editor = find_editor()
        .context("no editor found. Set $EDITOR, e.g. `EDITOR=nano mcsa config edit`")?;
    let draft = path.with_extension("edit.toml");
    config::write_private(&draft, original.as_bytes())?;

    let result = (|| loop {
        let mut parts = editor.split_whitespace();
        let status = Command::new(parts.next().unwrap())
            .args(parts)
            .arg(&draft)
            .status()
            .with_context(|| format!("couldn't start {editor}"))?;
        if !status.success() {
            bail!("{editor} exited with an error, so nothing was saved");
        }
        let text = fs::read_to_string(&draft)?;
        if text == original {
            println!("  No changes.");
            return Ok(());
        }
        match Config::parse(&text) {
            Ok(_) => {
                fs::rename(&draft, path)?;
                ui::ok(format!("Saved {}", pretty_path(path)));
                reload_hint();
                return Ok(());
            }
            Err(e) => {
                ui::fail(format!("That config has a problem: {e:#}"));
                let again = stdin().is_terminal()
                    && Confirm::new("Open the editor again to fix it?")
                        .with_default(true)
                        .prompt()?;
                if !again {
                    println!("  Discarded your changes. The old config is still in place.");
                    return Ok(());
                }
            }
        }
    })();
    let _ = fs::remove_file(&draft);
    result
}

fn find_editor() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|e| !e.trim().is_empty()))
        .or_else(|| {
            ["nano", "vim", "vi"]
                .into_iter()
                .find(|e| find_in_path(e).is_some())
                .map(String::from)
        })
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

fn reload_hint() {
    if service::is_installed() {
        println!("  Run {} to apply it.", ui::cyan("mcsa reload"));
    }
}

pub fn reload(path: &Path) -> Result<()> {
    // Validate first, so we never restart the service into a broken config.
    let cfg = Config::load(path)?;
    if !service::is_installed() {
        bail!(
            "the background service isn't set up. Run `mcsa setup`, or `mcsa run` to check in this terminal"
        );
    }
    {
        let _p = Pending::start("Restarting the background service…");
        service::restart()?;
    }
    ui::ok(format!(
        "Reloaded. Checking {} every {}",
        cfg.address(),
        format_duration(cfg.interval)
    ));
    Ok(())
}

pub fn status(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    let now = monitor::unix_now();
    let ago = |t: u64| match now.saturating_sub(t) {
        0..=59 => "just now".to_owned(),
        secs => format!("{} ago", human_duration(secs)),
    };

    let alerter = if !service::is_installed() {
        ui::yellow("not running in the background (set it up with `mcsa setup`)")
    } else {
        match service::active_state().as_str() {
            "active" | "activating" => ui::green("● running in the background"),
            "failed" => ui::red("● crashed. See why with `mcsa logs`"),
            other => ui::yellow(format!("○ {other}. Start it with `mcsa reload`")),
        }
    };
    let last = match monitor::load_state(&config::state_path(path)).filter(|s| s.host == cfg.host) {
        None => ui::dim("no checks yet"),
        Some(s) => format!(
            "{} since {}{}",
            if s.online {
                ui::green("up")
            } else {
                ui::red("down")
            },
            ago(s.since)
                .trim_end_matches(" ago")
                .replace("just now", "a moment ago"),
            if s.checked > 0 {
                ui::dim(format!(" (checked {})", ago(s.checked)))
            } else {
                String::new()
            }
        ),
    };
    let live = {
        let _p = Pending::start(format!("Pinging {}…", cfg.address()));
        monitor::ping_once(&cfg.host, cfg.port, cfg.timeout)
    };
    let right_now = match live {
        Ok(s) => format!("{} {}", ui::green("online"), ui::dim(monitor::describe(&s))),
        Err(e) => format!("{} {}", ui::red("not answering"), ui::dim(format!("{e:#}"))),
    };

    let server = match &cfg.name {
        Some(name) => format!("{} {}", ui::bold(name), ui::dim(cfg.address())),
        None => ui::bold(cfg.address()),
    };
    println!();
    ui::table(&[
        ("Server", server),
        ("Right now", right_now),
        ("Last check", last),
        ("Alerter", alerter),
        (
            "Settings",
            format!(
                "every {}, pings {} on DOWN",
                format_duration(cfg.interval),
                describe_mention(cfg.mention.as_deref())
            ),
        ),
    ]);
    println!();
    Ok(())
}

pub fn logs(follow: bool, lines: u32) -> Result<()> {
    let mut cmd = Command::new("journalctl");
    cmd.args([
        "--user-unit",
        service::UNIT,
        "--output",
        "short",
        "--lines",
        &lines.to_string(),
    ]);
    if follow {
        cmd.arg("--follow");
    }
    Err(cmd.exec()).context("couldn't run journalctl")
}

pub fn test_webhook(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    {
        let _p = Pending::start("Sending…");
        discord::send(&cfg.webhook_url, &discord::test(cfg.display_name()))?;
    }
    ui::ok("Sent a test message. Check your Discord channel");
    Ok(())
}

pub fn uninstall(path: &Path, purge: bool, yes: bool) -> Result<()> {
    let bin = service::bin_path()?;
    if !yes {
        if !stdin().is_terminal() {
            bail!("pass --yes to uninstall without being asked");
        }
        let question = if purge {
            "Remove the background service, the mcsa command and your settings?"
        } else {
            "Stop and remove the background service and the mcsa command?"
        };
        if !Confirm::new(question).with_default(false).prompt()? {
            println!("  Nothing changed.");
            return Ok(());
        }
    }

    let was_installed = service::is_installed();
    if was_installed {
        service::uninstall()?;
        ui::ok("Stopped and removed the background service");
    }
    if bin.exists() {
        fs::remove_file(&bin).with_context(|| format!("removing {}", pretty_path(&bin)))?;
        ui::ok(format!("Removed {}", pretty_path(&bin)));
    }
    if purge {
        for file in [path.to_owned(), config::state_path(path)] {
            match fs::remove_file(&file) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("removing {}", pretty_path(&file)));
                }
                _ => {}
            }
        }
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir(dir); // only succeeds if nothing else is in there
        }
        ui::ok("Deleted your settings");
    } else if path.exists() {
        println!(
            "  Your settings are still in {}. Use {} to delete them too.",
            pretty_path(path),
            ui::cyan("--purge")
        );
    }
    if was_installed {
        println!(
            "  {}",
            ui::dim(
                "Lingering is still on for your user. Turn it off with `loginctl disable-linger` if nothing else needs it."
            )
        );
    }
    Ok(())
}
