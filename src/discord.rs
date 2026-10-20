use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::ping::Status;

const RED: u32 = 0xED4245;
const GREEN: u32 = 0x57F287;
const BLURPLE: u32 = 0x5865F2;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn check_status(resp: &mut ureq::http::Response<ureq::Body>) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    match status.as_u16() {
        401 | 403 | 404 => bail!(
            "Discord doesn't recognize this webhook (it may have been deleted). Copy the URL from Discord again"
        ),
        429 => bail!("Discord is rate limiting this webhook, try again in a minute"),
        _ => bail!("Discord rejected the request ({status}): {}", body.trim()),
    }
}

pub fn send(webhook_url: &str, payload: &Value) -> Result<()> {
    let mut resp = agent()
        .post(webhook_url)
        .send_json(payload)
        .context("couldn't reach Discord")?;
    check_status(&mut resp)
}

/// Looks the webhook up without posting anything, and returns its name.
pub fn webhook_name(webhook_url: &str) -> Result<String> {
    let mut resp = agent()
        .get(webhook_url)
        .call()
        .context("couldn't reach Discord")?;
    check_status(&mut resp)?;
    let info: Value = resp
        .body_mut()
        .read_json()
        .context("that URL didn't answer like a Discord webhook")?;
    Ok(info["name"].as_str().unwrap_or("webhook").to_owned())
}

pub fn down(name: &str, error: &str, attempts: u32, now: u64, mention: Option<&str>) -> Value {
    let tries = if attempts == 1 {
        "1 ping".to_owned()
    } else {
        format!("{attempts} pings in a row")
    };
    message(
        mention,
        json!({
            "title": format!("🔴 {name} is DOWN"),
            "description": format!(
                "The server didn't answer {tries}.\n\n**Error:** `{}`\n**Detected:** <t:{now}:f>",
                error.replace('`', "'"),
            ),
            "color": RED,
        }),
    )
}

pub fn up(name: &str, status: &Status, down_since: u64, now: u64) -> Value {
    message(
        None,
        json!({
            "title": format!("🟢 {name} is back UP"),
            "description": format!(
                "**Downtime:** {} (since <t:{down_since}:f>)\n**Version:** {}\n**Players:** {}/{}",
                human_duration(now.saturating_sub(down_since)),
                status.version,
                status.players_online,
                status.players_max,
            ),
            "color": GREEN,
        }),
    )
}

pub fn test(name: &str) -> Value {
    message(
        None,
        json!({
            "title": "✅ Webhook works",
            "description": format!("This channel will get a message when **{name}** goes down and when it comes back."),
            "color": BLURPLE,
        }),
    )
}

fn message(mention: Option<&str>, embed: Value) -> Value {
    json!({
        "username": "Minecraft Server Alerter",
        "content": mention.unwrap_or(""),
        // Only let the configured mention ping anyone.
        "allowed_mentions": { "parse": if mention.is_some() { json!(["everyone", "roles", "users"]) } else { json!([]) } },
        "embeds": [embed],
    })
}

pub fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::human_duration;

    #[test]
    fn durations() {
        assert_eq!(human_duration(42), "42s");
        assert_eq!(human_duration(600), "10m");
        assert_eq!(human_duration(2 * 3600 + 13 * 60 + 9), "2h 13m");
        assert_eq!(human_duration(3 * 86400 + 4 * 3600), "3d 4h");
    }
}
