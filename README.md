<div align="center">

# discord-voice-tray

***Linux tray icon that mirrors your Discord voice status in real time — in channel, muted, or deafened.***

</div>

The Discord client for Linux shows a static tray icon: to know whether you are muted you have to open the window. This daemon watches Discord's local RPC socket and publishes a dynamic icon via StatusNotifierItem, just like the Windows client does out of the box. Zero configuration: start it, authorize once in Discord, and it works.

## How it works

A single process with two tasks: one connects to Discord's IPC socket, authenticates via OAuth (StreamKit, no app registration needed) and subscribes to voice events; the other reflects every change on the panel icon. The authorization popup appears only the first time — the token is cached in `~/.config/discord-voice-tray/`. If Discord closes, the daemon shows it and reconnects on its own when it comes back. Protocol and auth details in the [development guide](docs/development.md).

## States

| State           | Icon              | Meaning                          |
|-----------------|-------------------|----------------------------------|
| `DiscordClosed` | dimmed gray       | Discord is not running           |
| `Idle`          | light headset     | Connected, not in a voice channel |
| `VoiceUnmuted`  | green             | In channel, mic open             |
| `VoiceMuted`    | red crossed mic   | In channel, muted                |
| `VoiceDeafened` | crossed headphones | In channel, deafened            |

Deafened wins over muted; mute/deafen only matter inside a channel.

## Install

```bash
cargo build --release
```

Produces a single self-contained binary (`target/release/discord-voice-tray`) with the icons embedded. Requires the official Discord client (the browser version does not create the RPC socket) and a panel with StatusNotifierItem support: XFCE 4.16+, KDE Plasma, or GNOME with the AppIndicator extension.

## Quick start

1. With Discord open, run `./target/release/discord-voice-tray`.
2. Accept the authorization popup in Discord (first time only).
3. Done: the icon now mirrors your voice status. For autostart, copy `systemd/discord-voice-tray.service` to `~/.config/systemd/user/` and enable it with `systemctl --user enable --now discord-voice-tray`.

Verbose logs: `RUST_LOG=debug ./target/release/discord-voice-tray`. Common issues in [troubleshooting](docs/troubleshooting.md).

## Architecture

| Module          | Role                                                       |
|-----------------|------------------------------------------------------------|
| `src/ipc/`      | Socket, framing, RPC session (handshake/auth/subscribe) and reconnection with backoff |
| `src/state.rs`  | Pure `VoiceState` state machine, testable without a socket |
| `src/tray.rs`   | StatusNotifierItem (ksni): icon, tooltip and menu          |
| `src/config.rs` | Optional TOML config and OAuth token cache                 |
| `src/main.rs`   | Startup, `watch` channel between tasks and clean shutdown  |

Full code map, design rules and recipes for extending it (icons, events, states) in the [development guide](docs/development.md).

## License

[MIT](LICENSE) — use it, modify it and redistribute it freely.
