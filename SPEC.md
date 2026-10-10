# Spec — Independent agent input seats on Wayland/KWin

Revision 2 (2026-10-10). Supersedes the direction-only notes in [`CLAUDE.md`](CLAUDE.md)
where they conflict; the Phase 0 evidence is in [`docs/research/`](docs/research/).

## 1. Problem

An AI agent needs to operate GUI applications (type, click, scroll, look at the screen)
**without ever touching the human's pointer, keyboard focus or windows**, and the human
must be able to watch the agent work. Existing Wayland injection paths
(`zwp_virtual_keyboard_v1`, `zwlr_virtual_pointer_v1`, portal `RemoteDesktop` →
`ConnectToEIS`) all feed the *same* seat as the human, so they move the human's cursor
and steal focus. `ydotool`/`wtype` are explicitly out of scope for this reason.

### Requirements

| ID | Requirement |
|----|-------------|
| R1 | Agent input must never move the human's cursor, change the human's focus, or trigger the human's global shortcuts. |
| R2 | The agent has its own cursor, its own keyboard stream and its own focus. |
| R3 | The agent can see its desktop (screenshot / stream) and the human can watch live. |
| R4 | Agent-facing API is transport-neutral (CLI + MCP), so backends can change without changing agents. |
| R5 | Production code is Rust (project rule). Only an optional compositor fork may be C++. |
| R6 | Several agents can run concurrently, each isolated from the others. |
| R7 | Nothing here may modify the human's `~/.config` (nested compositor gets its own config dirs). |

## 2. Phase 0 outcome (feasibility gate)

KWin 6.7.5 source was read by two independent investigations (see `docs/research/`).

* The Wayland protocol layer is already multi-seat: `SeatInterface` is an ordinary class,
  each instance owns its own `PointerInterface`/`KeyboardInterface`, and clients can bind a
  second `wl_seat` (`src/wayland/seat.cpp`).
* The compositor core is **not**: exactly one `SeatInterface` is built
  (`wayland_server.cpp:401`); `waylandServer()->seat()` has ~210 call sites in 23 files;
  `InputRedirection` owns single keyboard/pointer/tablet/touch redirections and wires every
  device to them (`input.cpp:3238-3278`); `InputDevice` has no seat attribute;
  `Workspace::m_activeWindow` is a single pointer and keyboard focus is derived from it
  (`keyboard_input.cpp:249`); `Cursors`/`CursorItem` are singletons; the EIS plugin's
  "seat" is a libei concept and its devices land on the human's redirections.
* Estimate for a fork: ~2,000–2,500 LOC of invasive C++ plus ~500 LOC tests, a rebase on
  every KWin release, regression risk for the human's session, and **XWayland cannot be
  covered** (single X seat).

**Verdict:** a second seat inside the human's KWin is *feasible-with-large-cost*. It is
therefore *not* the first backend. Per the original gate ("revisit on evidence"), the
architecture below moves the fork to an optional track and ships an isolation-by-
construction backend first.

## 3. Architecture

```
            ┌────────────── agent (Claude / Gemini / Codex / …) ─────────────┐
            │  MCP (stdio)            or            CLI (input-director …)   │
            └───────────────┬─────────────────────────────┬──────────────────┘
                            │        seat API (R4)        │
                   ┌────────▼───────────────────────────────▼──────┐
                   │                 input-director                │
                   │  session mgr · input (EIS/libei) · capture    │
                   └───┬──────────────────────┬────────────────────┘
        Backend A: nested-kwin (default)   Backend C: kwin-fork (optional)
   private dbus-daemon + kwin_wayland --virtual      patched human KWin, extra wl_seat
   own seat/cursor/focus/desktop by construction     (Phase 2, gated)
```

### Backend A — `nested-kwin` (default, ships first)

One headless `kwin_wayland --virtual` per agent session, on a **private D-Bus session
bus**, with its own Wayland socket and private `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/
`XDG_CACHE_HOME`. Verified empirically on this host (KWin 6.7.5):

* `kwin_wayland --virtual --socket NAME --no-lockscreen --no-global-shortcuts` starts, and
  registers `org.kde.KWin`, `org.kde.KWin.ScreenShot2` on the private bus.
* `org.kde.KWin.EIS.RemoteDesktop.connectToEIS(capabilities) → (fd, cookie)` is exported on
  that bus (capabilities: keyboard=1, pointer=2, touch=4). Its EIS devices (relative
  pointer, absolute pointer with per-output regions, keyboard + XKB keymap) feed **only the
  nested compositor's seat**. R1/R2/R6 hold by process isolation.
* Screenshots: `ScreenShot2.CaptureWorkspace(options, pipe_fd)` (restricted interface;
  the nested KWin is started with `KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1`).
* Apps are launched into the session (`WAYLAND_DISPLAY=NAME`, private bus address).

Limitations (documented, accepted): apps must be launched *inside* the session (the agent
cannot drive windows that already exist in the human's session); GPU apps need a render
node; the human watches via a viewer (screenshot stream now, PipeWire/KRdp later).

### Backend C — `kwin-fork` (optional, Phase 2)

Only if the requirement "agent works on the human's *existing* windows concurrently"
becomes mandatory. Work items are the numbered patch plan in
[`docs/research/phase0-kwin-seat-study.md`](docs/research/phase0-kwin-seat-study.md) §7.
It sits behind the same seat API.

### Backend B — Smithay compositor (long-term, Rust)

A purpose-built Rust compositor with native multi-seat; it is the only path that satisfies
R5 for the compositor itself. Out of scope for now; the seat API keeps the door open.

## 4. Components

| Crate | Role |
|-------|------|
| `crates/seat-core` | Session manager (spawn/track/teardown `dbus-daemon` + `kwin_wayland`), D-Bus client for the private bus, EIS input driver (via `reis`), screenshot capture + PNG encode, XKB text→key mapping. |
| `crates/input-director` | Binary: `session start/stop/list`, `launch`, `screenshot`, `move`, `click`, `scroll`, `key`, `type`, `mcp` (stdio MCP server exposing the same operations as tools). |

### Session lifecycle & state

State dir: `$XDG_RUNTIME_DIR/input-director/<name>/` containing `session.json`
(`name, socket, bus_address, kwin_pid, dbus_pid, size, started_at, apps[]`), `bus`
(unix socket), `config/ state/ cache/`, `kwin.log`. All child PIDs are recorded so teardown
never relies on `pkill` (project rule: track and terminate by PID). Teardown sends SIGTERM
to apps, kwin, then dbus-daemon, and removes the state dir.

### Input model

* Pointer: absolute moves via the EIS absolute device (coordinates in the session's
  logical space), buttons and scroll via the same device; no dependence on previous state
  so every CLI call is stateless.
* Keyboard: raw evdev keycodes with the keymap delivered by EIS; `type` maps Unicode
  characters to (keycode, modifier set) with `xkbcommon` against that keymap.
* Each operation connects, performs the EIS handshake, sends frames with `start_emulating`
  / `stop_emulating`, and disconnects (the MCP server may keep the connection open).

### Safety properties (tested)

1. Moving/clicking/typing in session A does not change the human's pointer position
   (verified through the human compositor's cursor position before/after).
2. Session state dirs are `0700`; the private bus socket is user-only.
3. `~/.config` is untouched (checksum before/after in the integration test).
4. Teardown leaves no processes recorded in `session.json`.

## 5. Phases

* **P0 — Feasibility (done):** reports in `docs/research/`; empirical spike of backend A.
* **P1 — `seat-core` + `input-director` + MCP (this repo, first milestone).**
* **P2 — Viewer:** live view for the human (PipeWire/KRdp or MJPEG over loopback) and a
  multi-session dashboard.
* **P3 — Optional KWin fork** (backend C) — gated on a stated need for human-session
  windows.
* **P4 — Smithay backend** (backend B).

## 6. Open questions

1. Is "agent cannot touch pre-existing windows in the human's session" acceptable? If not,
   P3 becomes mandatory (and is XWayland-limited).
2. Viewer preference: a lightweight screenshot/MJPEG pane, or a real PipeWire/RDP stream.
