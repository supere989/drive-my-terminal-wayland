# drive-my-terminal-wayland

Give an AI agent its **own** mouse cursor, keyboard input stream, and independent focus on a Wayland desktop (KDE Plasma / KWin), running concurrently with the human user's desktop — **zero focus stealing, zero mouse hijacking, zero interference**.

## The Problem

Traditional automation tools (`wtype`, `ydotool`, `xdotool`, and standard Wayland virtual keyboard/pointer protocols) inject events into whatever window currently has focus on the active seat. When an AI agent moves the mouse or types, it moves the human user's physical mouse cursor and steals keyboard focus.

Wayland compositor protocols do not allow directing input to arbitrary unfocused windows. Multi-seat is the only architecture that provides true input isolation.

## The Solution

`drive-my-terminal-wayland` implements **Backend A (Isolated Virtual Seats)**:
- Creates an on-demand, headless `kwin_wayland --virtual` instance connected to an isolated private D-Bus session bus.
- Runs with its own Wayland socket (`idir-<name>`) and sandboxed configuration directories (`XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`).
- Input is injected via KWin's private `org.kde.KWin.EIS.RemoteDesktop` (libei / `reis`), reaching only the agent seat.
- Screen capture via KWin's `org.kde.KWin.ScreenShot2` produces high-resolution PNG snapshots (including the agent cursor).
- Child process PIDs are strictly tracked and terminated on teardown (`SIGTERM` followed by `SIGKILL`), leaving zero orphaned processes.

## Architecture & Verification

- **Feasibility Study:** Detailed in [`SPEC.md`](SPEC.md) and [`docs/research/phase0-kwin-seat-study.md`](docs/research/phase0-kwin-seat-study.md). An in-depth audit of KWin 6.7.5 source confirmed that while Wayland protocol supports multiple seats, KWin's compositor core hardcodes single seat throughout `InputRedirection`, `Workspace::m_activeWindow`, and cursor singletons. Backend A provides process-level isolation by construction.
- **Language:** Written 100% in safe Rust (workspace packages: `seat-core` and `input-director`).

## CLI Usage

### 1. Start a Seat
```bash
# Starts a 1280x800 virtual seat with its own cursor, keyboard, and focus
input-director session start agent1 --size 1280x800
```

### 2. Launch an Application into the Seat
```bash
# Runs the command inside the isolated seat environment
input-director launch agent1 konsole
```

### 3. Take a Screenshot
```bash
input-director screenshot agent1 --output agent-screen.png
```

### 4. Move, Click, Type, and Press Keys
```bash
# Move cursor
input-director move agent1 640 400

# Click (left, right, middle; optional --count for double-click)
input-director click agent1 640 400 --button left

# Type text into focused window
input-director type agent1 "echo hello world"

# Press key chord
input-director key agent1 Return
input-director key agent1 "ctrl+shift+t"

# Scroll (positive dy = down)
input-director scroll agent1 640 400 --dy 1
```

### 5. Stop the Seat
```bash
input-director session stop agent1
```

## Model Context Protocol (MCP) Server

`input-director` includes a built-in stdio MCP server for direct integration with AI agent frameworks (Claude Code, Antigravity, Codex, etc.):

```bash
input-director mcp
```

### Available MCP Tools:
- `seat_start`: Create an isolated agent seat session.
- `seat_stop`: Cleanly tear down a seat and its child processes.
- `seat_list`: List active seats and their running processes.
- `seat_launch`: Launch any GUI or CLI program inside the seat.
- `seat_screenshot`: Capture the seat's display as base64-encoded PNG.
- `seat_move`: Move the agent's cursor.
- `seat_click`: Click at coordinates within the seat.
- `seat_scroll`: Scroll wheel notches.
- `seat_key`: Press key combinations (`ctrl+shift+t`, `Return`, `alt+F4`).
- `seat_type`: Type strings character-by-character using the session keymap.

## Building and Testing

```bash
cargo build --release
cargo test --workspace
```
