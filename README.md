# mcsa: Minecraft Server Alerter

Get a Discord message when your Minecraft Java server goes **down**, and another when it's **back up**. You get one message per outage, not one every check.

```
$ mcsa status

  Server      My SMP play.example.com
  Right now   online Paper 1.21.4, 3/20 players, 42 ms via play.example.com:25565
  Last check  up since 3d 4h (checked 2m ago)
  Alerter     ● running in the background
  Settings    every 10m, pings @everyone on DOWN
```

- Uses the real Minecraft status ping, so a crashed server on a live machine still counts as down.
- Follows SRV records like the game does, so the address players type is all you need.
- Tries several times before declaring the server down, so a single dropped packet doesn't send an alert.
- A single ~3 MB static binary using under 2 MB of RAM, with no runtime dependencies.
- Runs as a systemd **user** service, so no root is needed.

## Requirements

- A Linux machine (x86_64 or aarch64) to run the alerter on. For the background service it needs systemd; without it you can still run `mcsa run` yourself.
- A Minecraft **Java Edition** server to watch.
- A Discord channel where you can create a webhook.

Run the alerter on a different machine than the Minecraft server if you can. If they share a machine and that machine goes down, nothing is left to send the alert.

## Install

### Download a release

Grab the binary for your architecture from the [latest release](../../releases/latest). `uname -m` tells you which one you need:

| `uname -m` | File |
|---|---|
| `x86_64` | `mcsa-x86_64-linux` |
| `aarch64` | `mcsa-aarch64-linux` |

```bash
mv mcsa-x86_64-linux mcsa
chmod +x mcsa
./mcsa setup
```

### Build from source

You need [Rust](https://rustup.rs) and a C compiler (`gcc`):

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/mcsa setup
```

To build an aarch64 binary on an x86_64 machine, install an aarch64 cross compiler, e.g. `gcc-aarch64-linux-gnu` and `libc6-dev-arm64-cross` on Debian/Ubuntu. Then build with `--target aarch64-unknown-linux-musl`. The binaries are statically linked, so you can build on one machine and copy the result to another.

## Setup

```bash
./mcsa setup
```

The setup wizard asks for the server address and your Discord webhook, and checks both live as you go. It then:

- saves your settings to `~/.config/minecraft-server-alerter/config.toml`,
- installs itself to `~/.local/bin/mcsa`,
- starts a background service that keeps running after you log out and starts again on boot.

Run `mcsa setup` again any time to change things. Your current settings are filled in for you.

To get a webhook URL in Discord: **Server Settings → Integrations → Webhooks → New Webhook**, pick the channel, then **Copy Webhook URL**.

### Scripted setup

```bash
mcsa setup --non-interactive \
  --host play.example.com \
  --webhook-url "https://discord.com/api/webhooks/…" \
  --mention @everyone \
  --interval 10m
```

Re-running setup (interactive or not) keeps any settings you don't pass. Add `--no-service` to only save the config, or `--skip-checks` to save without contacting the server and Discord first.

## Commands

| Command | What it does |
|---|---|
| `mcsa setup` | Interactive setup. Run it again any time to change things |
| `mcsa status` | Is the server up right now? Is the alerter running? |
| `mcsa config` | Show all settings |
| `mcsa config set <key> <value>` | Change one, e.g. `mcsa config set interval 5m` |
| `mcsa config unset <key>` | Reset one to its default |
| `mcsa config edit` | Open the file in `$EDITOR`. It's only saved if it's valid |
| `mcsa config path` | Print where the config file is |
| `mcsa reload` | Apply config changes to the background service |
| `mcsa logs [-f]` | Show (or follow) the background service's log |
| `mcsa test-webhook` | Post a test message to Discord |
| `mcsa run` | Check forever in the foreground (what the service runs) |
| `mcsa check` | Check once and exit |
| `mcsa uninstall [--purge]` | Remove the service and the `mcsa` command (`--purge` also deletes settings) |

Every command takes `--config <FILE>` (or `MCSA_CONFIG=<FILE>`) to use a different config file.

## Settings

| Key | Default | |
|---|---|---|
| `host` | *(required)* | The address players type. `mcsa config set host play.example.com:25566` sets the port too |
| `port` | auto | Unset = follow the SRV record, else 25565 |
| `name` | the host | Shown in Discord messages |
| `webhook_url` | *(required)* | The Discord webhook to post to. Keep it secret |
| `mention` | nobody | `@everyone`, `@here`, `<@&ROLE_ID>` or `<@USER_ID>`, added to DOWN alerts |
| `interval` | `10m` | How often to check (at least `30s`) |
| `attempts` | `3` | Failed pings in a row before it counts as down |
| `retry_delay` | `5s` | Wait between those pings |
| `timeout` | `5s` | How long each ping waits for an answer |

Role and user IDs need Discord's Developer Mode (**User Settings → Advanced**). Then right-click the role or user and choose **Copy Role ID** or **Copy User ID**.

After changing settings, run `mcsa reload` so the background service picks them up.

## When it alerts

| Last state | Now | Discord |
|---|---|---|
| none (first check) | up | nothing |
| none (first check) | down | 🔴 DOWN |
| up | down | 🔴 DOWN, with your mention |
| down | down | nothing |
| down | up | 🟢 back UP, with how long it was down |

If Discord can't be reached, the state isn't saved, so the next check sends the alert again. Changing `host` starts that server's history over.

## Releasing

1. Set the new `version` in `Cargo.toml` and run `cargo build` (this updates `Cargo.lock`).
2. Commit both with the message `Release vX.Y.Z`, matching that version. Any lines after the first become the release notes. A version like `1.0.0-beta.1` is published as a pre-release.
3. Push to the default branch.

The [release workflow](.github/workflows/release.yml) then runs the tests, builds static x86_64 and aarch64 binaries, and publishes release `vX.Y.Z` with the binaries and a `SHA256SUMS` file. If the commit's version doesn't match `Cargo.toml` or was already released, the workflow fails and says why. To check the build without releasing, run the workflow manually from the Actions tab.

## License

[MIT](LICENSE)
