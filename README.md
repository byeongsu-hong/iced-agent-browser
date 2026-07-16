# iced-agent-browser

Agent tooling for [iced](https://github.com/iced-rs/iced) desktop apps — the
`tauri-agent` experience for a toolkit that has no DOM to lean on:

- **Semantic tree** — `tree` / `find --role button --name Save` with `@ref`
  handles, built from lightweight `sem()` tags in your view code plus live
  layout bounds.
- **Drive** — `click @3`, `type`, `press --key k --mod ctrl`, hover, scroll,
  drag: synthetic events injected through iced's *real* input path (hit-test
  cursor included), headless-safe, no display-server input hacks.
- **Real OS accessibility** — the same tree is pushed to
  [AccessKit](https://github.com/AccessKit/accesskit) (AT-SPI / UIA / macOS
  AX), so screen readers see exactly what the agent sees, and screen-reader
  actions route through the same code as agent clicks.
- **Screenshots** (`window::screenshot` → PNG, WM-free), **log ring** (a
  `tracing` layer), curated **state** projections and **intents**,
  `wait`/`expect` polling, multi-window.
- **CLI + MCP** — a bun CLI and a zero-dependency stdio MCP server exposing
  16 `iced_*` tools.

Everything is dev-only by construction: the seams and the plugin compile under
`debug_assertions` (plus a cargo feature); a release binary contains none of
it — no listener, no endpoint file, no adapter.

## Layout

| Path | What |
|---|---|
| `iced-winit/` | Fork of crates.io `iced_winit 0.14.0` carrying exactly four `// AGENT SEAM` blocks: AccessKit adapter per window, `agent::set_tree`, `agent::inject` (+ hit-test cursor), ActionRequest channel. See `iced-winit/AGENT-FORK.md`. |
| `plugin/` | `iced-agent-plugin` crate: `sem()` tagging widget, Operation tree collector, loopback JSON-lines bridge + endpoint registry, tool handlers, log ring. |
| `plugin/bin/` | `iced-agent.ts` (CLI) and `iced-agent-mcp.ts` (stdio MCP server), zero npm dependencies — run directly with `bun`. |

## Wiring it into an iced app

Your app stays on stock `iced = "=0.14.0"`; the fork slips in underneath:

```toml
# workspace Cargo.toml
[patch.crates-io]
iced_winit = { path = "path/to/iced-agent-browser/iced-winit" }

# app Cargo.toml
[features]
default = ["agent"]
agent = ["iced_winit/agent", "dep:iced-agent-plugin"]

[dependencies]
iced_winit = { version = "=0.14.0", default-features = false }
iced-agent-plugin = { path = "path/to/iced-agent-browser/plugin", optional = true }
```

Then, gated on `all(feature = "agent", debug_assertions)`:

1. **Boot the bridge** once: `AgentHandle::boot("com.your.app", logs)` (get
   `logs` from `ring_layer()` when installing your `tracing` subscriber).
   The endpoint publishes to
   `${XDG_RUNTIME_DIR|TMPDIR|TMP}/iced-agent/<app-id>/endpoint.json`.
2. **Register windows**: `window_map().insert("main", id)` when a window opens.
3. **Tick** (a ~150 ms subscription): push the previous snapshot per window via
   `iced_winit::agent::set_tree(id, to_accesskit(&snapshot))`, refresh your
   curated `state_slot()` JSON, handle `drain_ui()` commands
   (`UiCommand::Intent` → your messages, `UiCommand::Shot` →
   `iced::window::screenshot` → PNG), and run
   `iced::advanced::widget::operate(Collector::new(snapshot_slot()))`.
4. **Tag views**: wrap window roots and interactive widgets —
   `sem(Role::Window, "main", content)`,
   `sem(Role::Button, label, button)`,
   `Sem::new(Role::TextInput, "query", input).value(text)`.
   Prefer wrapping shared builder functions: one wrap covers every call site.
5. **Route AccessKit actions**: take `iced_winit::agent::take_action_rx()` and
   convert `Action::Click` on `NodeId(n)` into the same synthetic click as the
   bridge (`@n` bounds → `agent::inject` cursor/press/release).

## Driving it

```bash
bun plugin/bin/iced-agent.ts tree
bun plugin/bin/iced-agent.ts find --role button --name Save
bun plugin/bin/iced-agent.ts click @3
bun plugin/bin/iced-agent.ts press --key k --mod ctrl
bun plugin/bin/iced-agent.ts state --path section
bun plugin/bin/iced-agent.ts shot --out /tmp/app.png
```

MCP (e.g. Claude Code `.mcp.json`):

```json
{ "mcpServers": { "iced-agent": { "command": "bun", "args": ["path/to/plugin/bin/iced-agent-mcp.ts"] } } }
```

`@ref` handles are valid until the next `tree`/`find` snapshot. The bridge is
loopback-only and unauthenticated by design (dev tool, curated command
surface); the endpoint dir is created `0700`.

## Rust-test selectors

The same `sem()` tags drive `iced_test` selectors, so a `Simulator` test
addresses a widget by role + name — not by its brittle visible text:

```rust
use iced_agent_plugin::selector::by;
use iced_agent_plugin::protocol::Role;

let mut ui = iced_test::simulator(app.view());
ui.click(by::role(Role::Button, "Save")).unwrap();   // case-insensitive exact name
// by::any(Role::TextInput) matches the first node of a role
assert!(app_saw(Msg::Saved, ui.into_messages()));
```

`by::role`/`by::any` return an `iced_selector::Selector` yielding a
`Target::Custom` carrying the `sem` node's bounds, which `click` uses directly.

## Headless QA on Linux (AT-SPI notes)

Hard-won quirks, each of which cost a debug round:

- `accesskit_unix` activates only on an `IsEnabled` **change** signal — flip
  `org.a11y.Status IsEnabled` to true *after* the app boots:
  `gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled '<true>'`
- Bare `dbus-run-session` sessions cannot dbus-activate the AT-SPI registry —
  start `/usr/libexec/at-spi2-registryd &` yourself, or accesskit's `Embed`
  fails and its event loop exits silently forever.
- If your dependency graph unifies `zbus` with its `tokio` feature (e.g. via
  `rfd`/`ashpd`), accesskit must ride the same reactor — this fork already
  pins `accesskit_winit` to tokio mode.
- Keep `XDG_RUNTIME_DIR` short (Wayland socket paths cap at 108 bytes).

## Versioning

`iced` and `iced_winit` are pinned `=0.14.0`; the fork keeps the upstream
version so `[patch.crates-io]` resolves graph-wide. On an iced bump,
re-vendor `iced_winit` and re-apply the seams (`AGENT-FORK.md` documents each
one; the diff is deliberately small and upstream-shaped).

## License

MIT. `iced-winit/` is a fork of `iced_winit` © Héctor Ramón and the Iced
contributors, also MIT.
