//! Installs mcsa as a systemd *user* service, so it runs under your account with no root,
//! and turns on lingering so it keeps running after logout and starts at boot.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, thread, time::Duration};

use anyhow::{Context, Result, bail};

use crate::config::{self, pretty_path};

pub const UNIT: &str = "mcsa.service";

pub fn unit_path() -> Result<PathBuf> {
    Ok(config::config_home()?.join("systemd/user").join(UNIT))
}

pub fn bin_path() -> Result<PathBuf> {
    Ok(config::home()?.join(".local/bin/mcsa"))
}

pub fn is_installed() -> bool {
    unit_path().is_ok_and(|p| p.exists())
}

fn systemctl(args: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("couldn't run systemctl. Is this a systemd-based Linux?")
}

fn systemctl_ok(args: &[&str]) -> Result<()> {
    let out = systemctl(args)?;
    if !out.status.success() {
        bail!(
            "`systemctl --user {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Whether we can talk to the user's systemd instance (not the case in containers, WSL1,
/// or `su` shells without a login session).
pub fn available() -> Result<()> {
    let out = systemctl(&["show-environment"])?;
    if !out.status.success() {
        bail!(
            "can't reach your user's systemd ({}). Log in over SSH directly as this user rather than using `su`",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// `active`, `inactive`, `failed`, `activating`…
pub fn active_state() -> String {
    systemctl(&["show", UNIT, "--property=ActiveState", "--value"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// Copies the running binary to ~/.local/bin/mcsa (unless it's already running from there).
pub fn install_binary() -> Result<PathBuf> {
    let target = bin_path()?;
    let current = env::current_exe().context("couldn't find the mcsa binary")?;
    if fs::canonicalize(&current).ok() == fs::canonicalize(&target).ok() {
        return Ok(target);
    }
    let dir = target.parent().unwrap();
    fs::create_dir_all(dir).with_context(|| format!("creating {}", pretty_path(dir)))?;
    // Copy then rename, so replacing a binary the service is running doesn't fail.
    let tmp = target.with_extension("new");
    fs::copy(&current, &tmp).with_context(|| format!("copying mcsa to {}", pretty_path(&tmp)))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, &target).with_context(|| format!("installing {}", pretty_path(&target)))?;
    Ok(target)
}

fn unit_file(bin: &Path, config_path: Option<&Path>) -> String {
    let env = config_path
        .map(|p| format!("Environment=\"MCSA_CONFIG={}\"\n", p.display()))
        .unwrap_or_default();
    format!(
        "# Installed by `mcsa setup`. Remove with `mcsa uninstall`.
[Unit]
Description=Minecraft Server Alerter

[Service]
ExecStart=\"{bin}\" run
{env}Restart=on-failure
RestartSec=30
NoNewPrivileges=yes

[Install]
WantedBy=default.target
",
        bin = bin.display()
    )
}

/// Writes the unit, enables it at login/boot, and (re)starts it.
/// `config_path` is only set when it isn't the default location.
pub fn install(bin: &Path, config_path: Option<&Path>) -> Result<()> {
    let path = unit_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, unit_file(bin, config_path))
        .with_context(|| format!("writing {}", pretty_path(&path)))?;
    systemctl_ok(&["daemon-reload"])?;
    systemctl_ok(&["enable", UNIT])?;
    restart()
}

/// Restarts the service, then waits a moment to make sure it stayed up.
pub fn restart() -> Result<()> {
    systemctl_ok(&["restart", UNIT])?;
    thread::sleep(Duration::from_millis(800));
    match active_state().as_str() {
        "active" | "activating" => Ok(()),
        state => bail!(
            "the service is {state} right after starting. See what went wrong with `mcsa logs`"
        ),
    }
}

pub fn uninstall() -> Result<()> {
    if is_installed() {
        // Ignore failures here: the unit may already be stopped or disabled.
        let _ = systemctl(&["disable", "--now", UNIT]);
        fs::remove_file(unit_path()?)?;
        systemctl_ok(&["daemon-reload"])?;
    }
    Ok(())
}

fn user_name() -> Option<String> {
    env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.is_empty())
}

pub fn linger_enabled() -> bool {
    let Some(user) = user_name() else {
        return false;
    };
    Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
}

pub fn enable_linger() -> Result<()> {
    let out = Command::new("loginctl")
        .arg("enable-linger")
        .output()
        .context("couldn't run loginctl")?;
    if !out.status.success() || !linger_enabled() {
        bail!(
            "{}. Run `sudo loginctl enable-linger {}` once to fix it",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .trim_end_matches('.'),
            user_name().unwrap_or_else(|| "$USER".into())
        );
    }
    Ok(())
}

/// Whether ~/.local/bin is on $PATH, so `mcsa` works as a command.
pub fn bin_dir_on_path() -> bool {
    let Ok(bin) = bin_path() else { return false };
    let dir = bin.parent().unwrap();
    env::var_os("PATH").is_some_and(|p| env::split_paths(&p).any(|d| d == dir))
}
