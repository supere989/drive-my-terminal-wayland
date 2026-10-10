# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

**Nothing is implemented yet.** This repo currently contains only this file. The
content below is the architecture direction agreed with the user, written so that
whichever session starts Phase 0 doesn't have to re-derive it. Do not assume any of
the directories or crates described below exist — check before referencing them.

## The two goals

1. Direct keyboard/mouse input to any process in the Wayland session.
2. A "Desktop Work Application and Control system": virtual desktops as first-class
   AI workspaces. An AI agent works inside one; the user switches to it to watch in
   realtime. This requires **Dual Simultaneous Input Control** — two or more
   independent input layers (human + AI) coexisting in the active session at the same
   time, across multi-monitor/multi-desktop configurations. This is *not* focus
   stealing (grab-then-release) — both input layers must be able to act concurrently
   without interfering with each other.

Goal 2 is the harder, novel part. Goal 1 turns out to be subsumed by it (see below).

## Key technical facts (established via research, not to be re-derived or assumed away)

- **Standard Wayland virtual-input protocols route through the existing seat's
  focus.** `zwp_virtual_keyboard_v1`, `zwlr_virtual_pointer_v1`, and the modern
  `xdg-desktop-portal` `RemoteDesktop.ConnectToEIS` + libei path all inject events that
  go wherever the compositor currently has focus. None of them let a client target an
  arbitrary *unfocused* window directly — you'd have to switch focus to it first
  ("steal, inject, restore"). This is why Goal 1, attempted in isolation with existing
  protocols, cannot give simultaneous/non-stealing behavior.
- **Multi-seat is the primitive that actually solves this.** Wayland/wlroots support
  multiple independent seats — each with its own pointer/keyboard and its own focus —
  coexisting in one session. This has been demonstrated on Sway (e.g. SDL's multi-seat
  support was tested there with two physical keyboard/mouse pairs held by two
  independent seats simultaneously). If a *synthetic-only* seat (no physical hardware,
  driven entirely by an AI-controlled process) can hold its own independent focus
  alongside the human's real seat, then Goal 1 and Goal 2 converge: the AI simply
  focuses windows on its own seat, and nothing needs to be "stolen" from the human.
- **The daily-driver compositor is KDE Plasma (KWin), not a wlroots compositor.**
  Unlike Sway, KWin has no documented multi-seat / independent-focus precedent found
  during research. This means the core feasibility question for Goal 2 is open and
  must be answered by reading KWin's own source, not by analogy to Sway/wlroots.
- **KWin already implements `ConnectToEIS`/libei today** (confirmed via KDE Connect's
  Wayland remote-input bridge projects, which use exactly this path). This gives Goal
  1 a working-today implementation path independent of any fork: inject via the
  existing portal RemoteDesktop session, accepting the "raise focus, then inject"
  limitation until Phase 2 lands.
- **One workspace is visible per output at a time.** "Watch the AI live" means either
  the AI gets a dedicated monitor, or the human time-multiplexes by switching to its
  virtual desktop/Activity. Two desktops simultaneously visible on one physical
  monitor is not something standard Plasma switching provides.
- **Decision, made explicitly by the user with the tradeoff disclosed:** fork and
  extend KWin itself, rather than the lower-risk alternative of building a separate
  nested compositor (e.g. with Smithay) that leaves KWin untouched. This is the
  higher-risk, higher-integration path — there is no known prior art for multi-seat in
  KWin, and it's a large, unfamiliar C++/Qt/KDE-Frameworks codebase. Don't second-guess
  this choice or quietly steer back toward the nested-compositor alternative; if
  Phase 0 proves KWin genuinely can't do this, that's the trigger to revisit — not a
  hunch.

## Phased architecture

**Phase 0 — Feasibility spike (gates everything else, not yet started).**
Clone KWin (`invent.kde.org/plasma/kwin`, GitHub mirror `github.com/KDE/kwin`) and
investigate its Wayland server / input-redirection code to answer one empirical
question: can KWin expose a second `wl_seat` driven only by synthetic input (via an
internal EIS/libei bridge or a custom virtual-input path), with focus routed
independently of the real seat, at least scoped to one virtual desktop/Activity? Do
not assume the answer or specific source file locations in advance — confirm by
reading the current KWin source. This is the highest-uncertainty part of the project.
Decision gate: success → proceed to Phase 2 as scoped; failure → revisit the
architecture (e.g. the nested-compositor alternative) rather than forcing it.

**Phase 1 — Goal 1 standalone tool (`input-director`).** Ships independently of
Phase 0/2 succeeding or not. A Rust CLI/daemon that injects input into a chosen window
today, using KWin's existing `org.freedesktop.portal.RemoteDesktop` →
`ConnectToEIS` → libei path. Its EIS client code is meant to be reused inside the
Phase 2 fork later — don't duplicate it.

**Phase 2 — KWin fork (dual-seat + AI-workspace binding).** Only start once Phase 0
has confirmed feasibility. Fork `KDE/kwin` on GitHub. Add: (a) creation of additional
seats not tied to physical udev devices; (b) scoping a seat's focus/input to a
specific KDE virtual desktop/Activity, so the AI seat only ever touches windows on
"its" desktop; (c) a DBus interface (KWin already exposes DBus extensively for
scripting/Activities — the natural integration point) to create/tear down these seats
and their desktop binding.

**Phase 3 — Control app ("Desktop Work Application and Control system").** A Rust
app/daemon that creates a KDE virtual desktop, requests an AI-bound seat from the
forked KWin over its Phase 2 DBus interface, launches the target agent process pinned
to that desktop+seat, and lets the human switch to/observe it in realtime via normal
Plasma virtual-desktop switching (plus optionally live thumbnails via KWin's existing
screenshot/screencast DBus interfaces, for a dashboard view without switching away).

## Planned repository layout (not yet created)

- `input-director/` — Rust crate, Phase 1's standalone CLI/daemon.
- `control-app/` — Rust crate, Phase 3's orchestration app.
- `kwin/` — the forked KWin checkout (submodule or sibling clone), Phase 2.

Language for everything outside the KWin fork itself (CLI tools, daemons, the control
app): Rust. The KWin fork stays in KWin's native C++/Qt/KDE-Frameworks stack.

## Working on this project

- Don't propose "just use `wtype`/`ydotool`/virtual-keyboard-protocol" as a solution
  to Goal 2 — that's the steal-then-inject model already ruled out above for the
  non-stealing, simultaneous requirement. It's fine for Phase 1's more modest scope.
- Phase 0's outcome is a hard dependency for Phase 2. Don't scaffold or write Phase 2
  code before Phase 0 has produced a concrete, verified answer from reading KWin's
  actual source.
- When Phase 0 begins, record its findings (exact files, classes, and whether
  multi-seat is feasible) back into this file so the decision gate is traceable.
