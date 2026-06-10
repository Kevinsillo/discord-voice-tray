# Development guide

How the code is organized and how to modify it without breaking its invariants.

## Code map

| File                  | Responsibility                                                                   |
|-----------------------|----------------------------------------------------------------------------------|
| `src/main.rs`         | Startup: config, `watch` channel, spawning the 2 tasks, shutdown (signals/Quit)  |
| `src/config.rs`       | Optional `config.toml` + OAuth token cache (0600). No networking                 |
| `src/state.rs`        | `VoiceState` and `reduce()`: **pure** reduction of raw signals to state. No I/O  |
| `src/ipc/socket.rs`   | Socket discovery (XDG/Flatpak/Snap//tmp × ipc-0..9) and framing. No RPC semantics |
| `src/ipc/protocol.rs` | Serde types for the RPC protocol (commands, events, responses). No I/O           |
| `src/ipc/client.rs`   | **One** session: handshake → auth → subscribe → event loop. Does not retry      |
| `src/ipc/mod.rs`      | `run_ipc_loop`: wraps the session with reconnection and backoff. Sole emitter of `DiscordClosed` |
| `src/tray.rs`         | `impl ksni::Tray`: state→icon/tooltip/menu mapping. The only place that knows about icons |

## Design rules

- **`state.rs` and `protocol.rs` are pure** (no I/O): that is what makes them testable without a socket. New state logic goes in `reduce()`, with tests.
- **Retries live only in `run_ipc_loop`**. `client.rs` returns a `Result` and ends; do not add reconnection loops inside the session.
- **The state→icon mapping lives only in `tray.rs`**.
- Shutdown is coordinated with a `CancellationToken` (`tokio-util`) shared between the Quit menu item, the IPC loop and `main`.
- The daemon never writes to Discord: `SET_VOICE_SETTINGS` and friends are deliberately out of scope.

## Protocol and authentication

- Socket framing: `[opcode u32 LE][length u32 LE][JSON UTF-8 payload]`. Opcodes: 0 HANDSHAKE, 1 FRAME, 2 CLOSE, 3 PING, 4 PONG.
- Session: handshake (`{"v":1,"client_id"}` → READY) → auth → `SUBSCRIBE` to `VOICE_SETTINGS_UPDATE`, `VOICE_CONNECTION_STATUS`, `VOICE_CHANNEL_SELECT` → `GET_VOICE_SETTINGS` + `GET_SELECTED_VOICE_CHANNEL` for the initial state → event loop.
- Default auth (Option A): `AUTHORIZE` with StreamKit's public `client_id` (`207646673902501888`) → Discord popup (first time only) → code exchange at `streamkit.discord.com/overlay/token` → `AUTHENTICATE`. Token cached at `~/.config/discord-voice-tray/token.json` (0600); if Discord rejects it, it is discarded and the flow runs again.
- Option B (your own OAuth app): `~/.config/discord-voice-tray/config.toml` with `client_id` and `client_secret`. Detection (`AuthMode`) exists in `config.rs`; the custom token exchange is pending (TODO in `client.rs::authenticate`) — today the daemon warns and falls back to StreamKit.
- Reconnection: exponential backoff 1s → 30s (reset after a valid session). Losing the socket publishes `DiscordClosed` immediately.

## Common modifications

**Changing the icons.** Edit the SVGs in `assets/svg/` and regenerate the PNGs in `assets/` (22 and 24 px). With `rsvg-convert`: `scripts/build-icons.sh`. Without a rasterizer: `scripts/gen-icons.py` (Python stdlib only). The PNGs are **embedded in the binary** (`include_bytes!` in `tray.rs`): rebuild after changing them. Note: ksni 0.3 expects ARGB32 in network byte order; the conversion from RGBA is documented in `tray.rs`.

**Listening to more RPC events** (e.g. `SPEAKING_START`). Add the type in `protocol.rs`, subscribe to it in `client.rs` (one `SUBSCRIBE` with its own nonce), translate the payload into signals in `apply_event` and extend `reduce()` in `state.rs` with tests.

**Adding states/icons.** New variant in `VoiceState` (`state.rs`), its priority in `reduce()`, its `label()`, its icon in `assets/` and the mapping in `tray.rs`.

**Tray menu actions.** The menu is built in `tray.rs` (`menu()`). Today it is read-only (status + Quit).

## Tests

```bash
cargo test
```

They cover framing (round-trip and errors), event parsing, `VoiceState` transitions and config/token handling. The parts with real I/O (socket, OAuth, tray) are verified manually: start the daemon with Discord open and check the transitions in the log (`RUST_LOG=debug`).
