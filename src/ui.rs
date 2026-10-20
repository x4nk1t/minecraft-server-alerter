//! Small helpers for friendly terminal output. Colors are skipped when stdout isn't a
//! terminal (e.g. under systemd) or NO_COLOR is set.

use std::fmt::Display;
use std::io::{IsTerminal, Write, stdout};
use std::sync::OnceLock;

fn colors() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, text: impl Display) -> String {
    if colors() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(text: impl Display) -> String {
    paint("1", text)
}
pub fn dim(text: impl Display) -> String {
    paint("2", text)
}
pub fn green(text: impl Display) -> String {
    paint("32", text)
}
pub fn red(text: impl Display) -> String {
    paint("31", text)
}
pub fn yellow(text: impl Display) -> String {
    paint("33", text)
}
pub fn cyan(text: impl Display) -> String {
    paint("36", text)
}

pub fn ok(msg: impl Display) {
    println!("  {} {msg}", green("✓"));
}
pub fn warn(msg: impl Display) {
    println!("  {} {msg}", yellow("!"));
}
pub fn fail(msg: impl Display) {
    println!("  {} {msg}", red("✗"));
}

/// A "Checking…" line that gets replaced by the result on a terminal.
pub struct Pending;

impl Pending {
    pub fn start(msg: impl Display) -> Pending {
        if colors() {
            print!("  {} {}", dim("…"), dim(msg));
            let _ = stdout().flush();
        }
        Pending
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if colors() {
            print!("\r\x1b[2K");
            let _ = stdout().flush();
        }
    }
}

/// Prints `label  value` rows with the labels lined up.
pub fn table(rows: &[(&str, String)]) {
    let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    for (label, value) in rows {
        println!("  {}  {value}", dim(format!("{label:<width$}")));
    }
}
