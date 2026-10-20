//! The config file (~/.config/minecraft-server-alerter/config.toml) and parsers for the
//! values people type in: addresses, durations, mentions.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Deserializer};

pub const DIR_NAME: &str = "minecraft-server-alerter";

/// Keys accepted by `mcsa config get/set/unset`, in display order.
pub const KEYS: [&str; 9] = [
    "host",
    "port",
    "name",
    "webhook_url",
    "mention",
    "interval",
    "attempts",
    "retry_delay",
    "timeout",
];

const MIN_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub name: Option<String>,
    pub webhook_url: String,
    #[serde(default)]
    pub mention: Option<String>,
    #[serde(default = "default_interval", deserialize_with = "de_duration")]
    pub interval: Duration,
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_retry_delay", deserialize_with = "de_duration")]
    pub retry_delay: Duration,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
}

fn default_interval() -> Duration {
    Duration::from_secs(600)
}
fn default_attempts() -> u32 {
    3
}
fn default_retry_delay() -> Duration {
    Duration::from_secs(5)
}
fn default_timeout() -> Duration {
    Duration::from_secs(5)
}

pub fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("$HOME is not set")
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> Result<PathBuf> {
    match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => Ok(home()?.join(".config")),
    }
}

pub fn default_path() -> Result<PathBuf> {
    Ok(config_home()?.join(DIR_NAME).join("config.toml"))
}

/// The up/down state lives next to the config so everything is in one folder.
pub fn state_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("state.json")
}

/// Shows paths under $HOME as `~/…`.
pub fn pretty_path(path: &Path) -> String {
    match home()
        .ok()
        .and_then(|h| path.strip_prefix(h).ok().map(Path::to_owned))
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

impl Config {
    pub fn new(host: String, port: Option<u16>, webhook_url: String) -> Config {
        Config {
            host,
            port,
            name: None,
            webhook_url,
            mention: None,
            interval: default_interval(),
            attempts: default_attempts(),
            retry_delay: default_retry_delay(),
            timeout: default_timeout(),
        }
    }

    pub fn load(path: &Path) -> Result<Config> {
        Self::load_optional(path)?.with_context(|| {
            format!(
                "there's no config at {} yet. Run `mcsa setup` first",
                pretty_path(path)
            )
        })
    }

    pub fn load_optional(path: &Path) -> Result<Option<Config>> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .with_context(|| format!("{} has a problem", pretty_path(path)))
                .map(Some),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", pretty_path(path))),
        }
    }

    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        validate_host(&self.host)?;
        ensure!(self.port != Some(0), "port must be between 1 and 65535");
        validate_webhook_url(&self.webhook_url)?;
        if let Some(mention) = &self.mention {
            validate_mention(mention)?;
        }
        ensure!(
            self.interval >= MIN_INTERVAL,
            "interval must be at least {}",
            format_duration(MIN_INTERVAL)
        );
        ensure!(
            (1..=10).contains(&self.attempts),
            "attempts must be between 1 and 10"
        );
        ensure!(
            (1..=60).contains(&self.timeout.as_secs()),
            "timeout must be between 1s and 60s"
        );
        ensure!(
            self.retry_delay.as_secs() <= 300,
            "retry_delay must be at most 5m"
        );
        Ok(())
    }

    /// Writes the config readable only by you, since the webhook URL is a secret.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .context("config path has no parent directory")?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", pretty_path(dir)))?;
        write_private(path, self.render().as_bytes())
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.host)
    }

    /// The address as a player would type it.
    pub fn address(&self) -> String {
        match self.port {
            Some(port) if self.host.contains(':') => format!("[{}]:{port}", self.host),
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn render(&self) -> String {
        let quote = |s: &str| toml::Value::String(s.to_owned()).to_string();
        let optional = |key: &str, value: Option<String>, example: &str| match value {
            Some(v) => format!("{key} = {v}"),
            None => format!("# {key} = {example}"),
        };
        format!(
            r#"# Minecraft Server Alerter (mcsa) settings.
# Change with `mcsa config set <key> <value>` or `mcsa config edit`, then run `mcsa reload`.

# The address players type into Minecraft.
host = {host}
# Leave unset to follow the SRV record like the game does (falls back to 25565).
{port}
# Name shown in Discord messages. Defaults to the host.
{name}

# Discord: Server Settings > Integrations > Webhooks > Copy Webhook URL. Keep this secret.
webhook_url = {webhook_url}
# Who to ping on DOWN alerts: "@everyone", "@here", "<@&ROLE_ID>" or "<@USER_ID>".
{mention}

# How often to check.
interval = "{interval}"
# The server counts as down after this many failed pings in a row, this far apart.
attempts = {attempts}
retry_delay = "{retry_delay}"
# How long each ping waits for an answer.
timeout = "{timeout}"
"#,
            host = quote(&self.host),
            port = optional("port", self.port.map(|p| p.to_string()), "25565"),
            name = optional("name", self.name.as_deref().map(quote), "\"My SMP\""),
            webhook_url = quote(&self.webhook_url),
            mention = optional(
                "mention",
                self.mention.as_deref().map(quote),
                "\"@everyone\""
            ),
            interval = format_duration(self.interval),
            attempts = self.attempts,
            retry_delay = format_duration(self.retry_delay),
            timeout = format_duration(self.timeout),
        )
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "host" => self.host.clone(),
            "port" => self.port.map(|p| p.to_string()).unwrap_or_default(),
            "name" => self.name.clone().unwrap_or_default(),
            "webhook_url" => self.webhook_url.clone(),
            "mention" => self.mention.clone().unwrap_or_default(),
            "interval" => format_duration(self.interval),
            "attempts" => self.attempts.to_string(),
            "retry_delay" => format_duration(self.retry_delay),
            "timeout" => format_duration(self.timeout),
            _ => return Err(unknown_key(key)),
        })
    }

    /// Sets a key from user input. Setting `host` also sets the port (or resets it to
    /// automatic), since players type them together.
    /// Leaves the config untouched if the new value is invalid.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let mut next = self.clone();
        next.apply(key, value.trim())?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "host" => (self.host, self.port) = parse_address(value)?,
            "port" => {
                self.port = match value {
                    "auto" | "" => None,
                    p => Some(parse_port(p)?),
                }
            }
            "name" => self.name = Some(value.to_owned()).filter(|n| !n.is_empty()),
            "webhook_url" => self.webhook_url = value.to_owned(),
            "mention" => {
                self.mention = match value {
                    "" | "none" => None,
                    m => Some(m.to_owned()),
                }
            }
            "interval" => self.interval = parse_duration(value)?,
            "attempts" => {
                self.attempts = value
                    .parse()
                    .map_err(|_| anyhow!("attempts must be a whole number"))?
            }
            "retry_delay" => self.retry_delay = parse_duration(value)?,
            "timeout" => self.timeout = parse_duration(value)?,
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<()> {
        match key {
            "host" | "webhook_url" => {
                bail!("{key} is required. Use `mcsa config set {key} …` to change it")
            }
            "port" => self.port = None,
            "name" => self.name = None,
            "mention" => self.mention = None,
            "interval" => self.interval = default_interval(),
            "attempts" => self.attempts = default_attempts(),
            "retry_delay" => self.retry_delay = default_retry_delay(),
            "timeout" => self.timeout = default_timeout(),
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }
}

fn unknown_key(key: &str) -> anyhow::Error {
    anyhow!(
        "unknown setting {key:?}. Valid settings: {}",
        KEYS.join(", ")
    )
}

/// Writes a file with 0600 permissions via a temp file + rename, so it's never half-written.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", pretty_path(&tmp)))?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", pretty_path(path)))
}

/// Parses what a player would type: `host`, `host:port`, an IP, or `[ipv6]:port`.
pub fn parse_address(input: &str) -> Result<(String, Option<u16>)> {
    let s = input.trim();
    ensure!(
        !s.is_empty(),
        "enter a server address, like play.example.com"
    );
    ensure!(
        !s.contains("://"),
        "enter just the address, like play.example.com"
    );
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .context("missing ] after the IPv6 address")?;
        let port = match after {
            "" => None,
            p => Some(
                p.strip_prefix(':')
                    .context("expected :port after the IPv6 address")?,
            ),
        };
        (host, port)
    } else if s.matches(':').count() == 1 {
        let (host, port) = s.split_once(':').unwrap();
        (host, Some(port))
    } else {
        (s, None)
    };
    validate_host(host)?;
    Ok((host.to_owned(), port.map(parse_port).transpose()?))
}

fn parse_port(s: &str) -> Result<u16> {
    s.trim()
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)
        .with_context(|| format!("{s:?} isn't a valid port (1-65535)"))
}

fn validate_host(host: &str) -> Result<()> {
    ensure!(
        !host.is_empty(),
        "enter a server address, like play.example.com"
    );
    let hostname_ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ensure!(
        hostname_ok || host.parse::<IpAddr>().is_ok(),
        "{host:?} doesn't look like a server address"
    );
    Ok(())
}

pub fn validate_webhook_url(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("https://") || url.starts_with("http://"),
        "the webhook URL should look like https://discord.com/api/webhooks/…"
    );
    Ok(())
}

pub fn is_discord_webhook(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    matches!(
        host,
        "discord.com" | "discordapp.com" | "ptb.discord.com" | "canary.discord.com"
    ) && path.starts_with("api/")
        && path.contains("webhooks/")
}

/// Hides the token half of a webhook URL for display.
pub fn mask_webhook_url(url: &str) -> String {
    match url.trim_end_matches('/').rsplit_once('/') {
        Some((base, token)) if token.len() > 8 => format!("{base}/{}…", &token[..4]),
        _ => url.to_owned(),
    }
}

pub fn validate_mention(mention: &str) -> Result<()> {
    let is_id_mention = ["<@&", "<@!", "<@"]
        .iter()
        .find_map(|prefix| mention.strip_prefix(prefix))
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    ensure!(
        is_id_mention || matches!(mention, "@everyone" | "@here"),
        "mention must be @everyone, @here, <@&ROLE_ID> or <@USER_ID>"
    );
    Ok(())
}

pub fn describe_mention(mention: Option<&str>) -> String {
    match mention {
        None => "nobody".into(),
        Some(m) if m.starts_with("<@&") => format!("role {}", &m[3..m.len() - 1]),
        Some(m) if m.starts_with("<@") => format!(
            "user {}",
            m.trim_start_matches("<@")
                .trim_start_matches('!')
                .trim_end_matches('>')
        ),
        Some(m) => m.into(),
    }
}

/// Parses `30s`, `10m`, `1h30m`, `2d`, or a bare number of seconds.
pub fn parse_duration(input: &str) -> Result<Duration> {
    let s = input.trim();
    let invalid = || anyhow!("{s:?} isn't a duration. Use something like 30s, 10m or 1h");
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    ensure!(!s.is_empty(), "enter a duration, like 10m");
    let mut total = 0u64;
    let mut number = String::new();
    for c in s.chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86400,
            _ => return Err(invalid()),
        };
        let n: u64 = number.parse().map_err(|_| invalid())?;
        total = n
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(invalid)?;
        number.clear();
    }
    ensure!(
        number.is_empty(),
        "{s:?} is missing a unit at the end (s, m, h or d)"
    );
    Ok(Duration::from_secs(total))
}

pub fn format_duration(d: Duration) -> String {
    let mut secs = d.as_secs();
    if secs == 0 {
        return "0s".into();
    }
    let mut out = String::new();
    for (unit, size) in [("d", 86400), ("h", 3600), ("m", 60), ("s", 1)] {
        if secs >= size {
            out += &format!("{}{unit}", secs / size);
            secs %= size;
        }
    }
    out
}

fn de_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seconds(u64),
        Text(String),
    }
    match Raw::deserialize(d)? {
        Raw::Seconds(s) => Ok(Duration::from_secs(s)),
        Raw::Text(t) => parse_duration(&t).map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://discord.com/api/webhooks/123456/abcdefghijklmnop";

    #[test]
    fn render_parses_back_to_the_same_config() {
        let mut c = Config::new("play.example.com".into(), None, URL.into());
        assert_eq!(Config::parse(&c.render()).unwrap(), c);

        c.port = Some(25566);
        c.name = Some("My \"SMP\"".into());
        c.mention = Some("<@&42>".into());
        c.interval = Duration::from_secs(5400);
        assert_eq!(Config::parse(&c.render()).unwrap(), c);
    }

    #[test]
    fn minimal_file_gets_defaults() {
        let c = Config::parse(&format!("host = \"a.b\"\nwebhook_url = \"{URL}\"")).unwrap();
        assert_eq!(c.interval, Duration::from_secs(600));
        assert_eq!(c.attempts, 3);
        assert_eq!(c.port, None);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(Config::parse("host = \"a.b\"").is_err()); // no webhook
        assert!(
            Config::parse(&format!(
                "host = \"a.b\"\nwebhook_url = \"{URL}\"\ninterval = \"5s\""
            ))
            .is_err()
        );
        assert!(
            Config::parse(&format!(
                "host = \"a.b\"\nwebhook_url = \"{URL}\"\ntypo = 1"
            ))
            .is_err()
        );
    }

    #[test]
    fn set_and_unset() {
        let mut c = Config::new("a.b".into(), None, URL.into());
        c.set("host", "mc.example.com:25570").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("mc.example.com", Some(25570)));
        c.set("host", "other.example.com").unwrap();
        assert_eq!(c.port, None, "a host without a port means automatic");
        c.set("interval", "1h").unwrap();
        assert_eq!(c.interval, Duration::from_secs(3600));
        assert!(c.set("interval", "10s").is_err());
        assert!(c.set("mention", "everyone").is_err());
        c.set("mention", "@here").unwrap();
        c.unset("mention").unwrap();
        assert_eq!(c.mention, None);
        assert!(c.unset("host").is_err());
        assert!(c.set("nope", "1").is_err());
    }

    #[test]
    fn addresses() {
        assert_eq!(
            parse_address(" play.example.com ").unwrap(),
            ("play.example.com".into(), None)
        );
        assert_eq!(
            parse_address("1.2.3.4:25566").unwrap(),
            ("1.2.3.4".into(), Some(25566))
        );
        assert_eq!(
            parse_address("[2001:db8::1]:25565").unwrap(),
            ("2001:db8::1".into(), Some(25565))
        );
        assert_eq!(
            parse_address("2001:db8::1").unwrap(),
            ("2001:db8::1".into(), None)
        );
        assert!(parse_address("").is_err());
        assert!(parse_address("play.example.com:0").is_err());
        assert!(parse_address("play.example.com:abc").is_err());
        assert!(parse_address("https://play.example.com").is_err());
        assert!(parse_address("my server").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert!(parse_duration("1h30").is_err());
        assert!(parse_duration("10 minutes").is_err());
        assert_eq!(format_duration(Duration::from_secs(5400)), "1h30m");
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
    }

    #[test]
    fn mentions() {
        for ok in ["@everyone", "@here", "<@&123>", "<@123>", "<@!123>"] {
            assert!(validate_mention(ok).is_ok(), "{ok}");
        }
        for bad in ["everyone", "<@&>", "<@&12a>", "@someone"] {
            assert!(validate_mention(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn webhook_helpers() {
        assert!(is_discord_webhook(URL));
        assert!(!is_discord_webhook("https://example.com/api/webhooks/1/2"));
        assert_eq!(
            mask_webhook_url(URL),
            "https://discord.com/api/webhooks/123456/abcd…"
        );
    }
}
