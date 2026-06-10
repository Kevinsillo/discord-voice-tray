# Troubleshooting

**The icon does not show up.** Check that your panel supports StatusNotifierItem: `busctl --user list | grep StatusNotifier` must show a `StatusNotifierWatcher`. XFCE 4.16+ and KDE Plasma ship it; on GNOME install the AppIndicator extension.

**It does not connect to Discord.** `$XDG_RUNTIME_DIR/discord-ipc-0` (or the Flatpak/Snap variants) must exist. Only the official desktop client creates that socket — Discord in the browser does not.

**The authorization popup shows up again.** The cached token expired or was revoked; the daemon re-authorizes on its own and refreshes the cache. To force a re-authorization: delete `~/.config/discord-voice-tray/token.json`.

**The icon does not change when muting/deafening.** By design, mute and deafen only affect the icon **inside** a voice channel; outside a channel the state stays "connected". If you are in a channel and it still does not react, start with `RUST_LOG=debug` and check whether `VOICE_SETTINGS_UPDATE` events are being logged.

**The official Discord icon is redundant.** Hide it in your panel's tray configuration. On XFCE it may be listed as "Google Chrome": that is the internal name Electron registers for its legacy icon.

**Discord was closed and the icon is slow to react when reopening.** Reconnection uses exponential backoff capped at 30 s; after several failed attempts the next one can take up to half a minute.
