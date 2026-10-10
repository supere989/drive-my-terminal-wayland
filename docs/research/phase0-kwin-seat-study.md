# Phase 0 — KWin 6.7.5 second-seat study

Read-only study of `kwin` tag `v6.7.5` (paths relative to `src/`). Produced by a research
subagent; independently cross-checked by an AGY (Gemini 3.8 Flash) run — see
`phase0-agy-report.md` if present.

**Verdict:** a second synthetic `wl_seat` is feasible at the protocol layer, but not without a
large, invasive fork of the compositor core. Recommended first path: per-agent nested
`kwin_wayland --virtual`.

## 1. Protocol layer (`wayland/*`) is already multi-seat
- `SeatInterface` is an ordinary class (one `wl_seat` global per instance, `wayland/seat.cpp:52`);
  its constructor registers itself in `DisplayPrivate::seats` (`seat.cpp:95-101`, `display_p.h:45`).
- Each seat owns its own `PointerInterface`, `KeyboardInterface`, `TouchInterface`, text-input
  (`seat.cpp:55-58, 337-379`); `seat_get_pointer`/`seat_get_keyboard` attach resources to that
  seat's own objects (`seat.cpp:72-83`) → clients can bind a second `wl_seat`.
- Protocols resolve the seat from the client's resource via `SeatInterface::get(res)`
  (`seat.cpp:411`): data device, primary selection, text-input, xdg-shell move/resize/popup grab
  (`xdgshell.cpp:450,463,510,850`), idle, shortcuts-inhibit, tablet, xdg-activation.
- Only ONE seat is constructed: `wayland_server.cpp:401`, exposed as `WaylandServer::seat()`
  (`wayland_server.h:94`, member `:262`).
- A per-client global filter exists (`wayland/filtered_display.cpp:29,46`; `allowInterface` at
  `wayland_server.cpp:154`) but filters by interface *name* only; per-`wl_global*` filtering
  would be ~20 LOC.

## 2. Single-seat hard-coding
- `waylandServer()->seat()`: 210 call sites in 23 files (`input.cpp` 70, `inputmethod.cpp` 46,
  `pointer_input.cpp` 24, xwayland ~25, `scene/workspacescene.cpp` 5, …).
- `InputRedirection` owns exactly one keyboard/pointer/tablet/touch redirection
  (`input.cpp:2927-2930`, `input.h:298-301`); `addInputDevice` wires *every* device to them
  (`input.cpp:3238-3278`).
- `InputDevice` (`core/inputdevice.h:94-137`) has **no seat attribute**; `session()->seat()` is
  only a logind string.
- Global filter chain (`processFilters`) means global shortcuts, lockscreen, screen edges,
  move/resize and tabbox all see any event.
- Keyboard focus = `workspace()->activeWindow()` (`keyboard_input.cpp:~245`, `update()` at
  `:249-263`); pointer constraints tied to `Workspace::windowActivated` (`pointer_input.cpp:~660`).

## 3. Activation assumes one seat
- `Workspace::m_activeWindow` single pointer (`workspace.h:657`); `setActiveWindow`
  (`activation.cpp:223-279`); `Window::isActive()` is a plain bool (`window.h:985`) mapped to
  xdg `Activated` (`xdgshellwindow.cpp:870-873`) — per window, so two "active" windows are
  protocol-legal.
- `requestFocus` (`activation.cpp:366-404`) ends in `setActiveWindow`; an agent path needs a
  parallel `m_agentActiveWindow` + focus chain; all `isActive()` consumers need an audit.
- Virtual desktops are per-output in 6.7 (`virtualdesktops.cpp:558-565`, `window.cpp:817`):
  agent windows would need a dedicated (virtual) output whose current desktop is the agent
  desktop.

## 4. Cursor is a global singleton
- `Cursors` has one `m_mouse` + `m_currentCursor` (`cursor.h:155,163,184-185`); one `CursorItem`
  in `WorkspaceScene` (`workspacescene.cpp:93,157-160`); compositor special-cases `CursorItem`
  for hardware cursor planes (`compositor.cpp:469,476,558,758`); screencast embeds only
  `currentCursor()` (`plugins/screencast/screencaststream.cpp:602`); `CursorImage` bound to the
  single seat (`pointer_input.cpp:1110-1134, 1311`).

## 5. The existing EIS/libei path does not solve it
- The EIS "seat" is a libei concept per EIS client (`eiscontext.cpp:129-138`), not a `wl_seat`.
  Devices (`eisbackend.cpp:147-199`) go through `InputBackend::deviceAdded` →
  `InputRedirection::addInputDevice` (`input.cpp:3363-3364`) and land on the **same** single
  redirections as physical devices → they move the human's pointer and steal focus.

## 6. Toolkits
Qt and GTK bind every `wl_seat` but treat the first as default; Qt has one app-wide focus
window and cursor (fine: agent apps are separate processes). Xwayland/Electron-X11 paths are
single-seat. Prior art for logical multi-seat in one session: Weston, wlroots/Sway, River
(designed multi-seat from the start).

## 7. Minimal patch plan for a KWin fork (backend C)
1. `AgentSeat` holding a 2nd `SeatInterface` (`agent0`) with own pointer/keyboard (~100 LOC).
2. `InputDevice::seatId` + seat-aware `addInputDevice` routing (~150 LOC).
3. `AgentInputRedirection` (~700–1000 LOC): own pointer position/hit-test on the agent
   output/desktop, own xkb state, bypassing the global filter chain.
4. `Workspace::m_agentActiveWindow` + parallel focus path (~300 LOC + `isActive()` audit).
5. Second software-composited `CursorItem` + `CursorImage` for the agent seat (~250 LOC).
6. Per-`wl_global*` client filter (~30 LOC).
7. Out of scope/impossible: XWayland; DnD ~150, data-device ~100, tests ~500.

Total ≈ 1,800–2,500 LOC core + ~500 tests; rebase on each KWin release; upstream unlikely to
accept; regression risk for the human session; not Rust.

## 8. Alternatives, ranked
1. **Nested `kwin_wayland --virtual` per agent** (recommended). `main_wayland.cpp` exposes
   `--virtual --width --height --scale --output-count --socket --no-lockscreen
   --no-global-shortcuts --exit-with-session`. Own bus ⇒ own seat/cursor/focus/desktop; input
   via that session's `org.kde.KWin.EIS.RemoteDesktop`; capture via `ScreenShot2`/PipeWire.
   Prior art: `isac322/kwin-mcp`. Limits: apps must be launched inside the session.
2. **Smithay nested compositor** — native multi-seat, Rust, but 5–15k LOC of shell work.
3. **KWin fork with extra seat** — not recommended first (cost/risk above).

## Empirical confirmation (this repo, 2026-10-10)
On the host KWin 6.7.5, `kwin_wayland --virtual --socket agentN` under a private
`dbus-run-session` registered `org.kde.KWin`, `org.kde.KWin.ScreenShot2` and exported
`/org/kde/KWin/EIS/RemoteDesktop` with `connectToEIS(i) → (h, i)`; Konsole launched into the
socket. See `scripts/spike-nested-kwin.sh`.
