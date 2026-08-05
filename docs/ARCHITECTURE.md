# Architecture

This document records the boundaries between layers and *why* each boundary is where it is. If a
change requires crossing one of these boundaries, that is a design discussion, not a refactor.

## The one rule

**Only the three presentation crates may depend on a GUI toolkit: `term-render`, `ui-chrome` and
`app-ui`. Nothing else.**

Protocols, the session model, the vault, terminal state and file transfers do not know that a GUI
exists. This is not architectural purity for its own sake — it is the project's insurance policy
against its single largest technical bet. `egui` was chosen because reproducing MobaXterm's chrome
pixel-for-pixel means drawing every pixel ourselves, and an immediate-mode framework with a full
`Painter` API is the best fit for that. If that bet turns out wrong, the cost of changing it is
rewriting three crates, not rewriting the product.

Even inside the presentation layer the toolkit is kept at arm's length where it costs nothing to do
so. `term-render`'s run-grouping (`runs.rs`) and key encoding (`keys.rs`) are pure logic with their
own tests; only the painting and the `egui::Key` adapter touch the toolkit. That is why the trickiest
parts of rendering — where wide glyphs break a run, what byte sequence Ctrl+Backspace sends — are
verifiable without a display.

If you add `egui` to a fourth crate's manifest, that is a design change, not a convenience.

## Two protocol abstractions, not one

Remote-access protocols split cleanly into two families, and collapsing them into one trait produces
an abstraction that fits neither.

```
        ┌──────────────────────── app-ui ────────────────────────┐
        │   Pane  ──  holds EITHER a TerminalPane OR a SurfacePane │
        └───────┬─────────────────────────────────┬──────────────┘
                │                                 │
      ┌─────────▼─────────┐             ┌─────────▼──────────┐
      │  Transport        │             │ GraphicalSurface   │
      │  bytes + resize   │             │ frames + input     │
      ├───────────────────┤             ├────────────────────┤
      │ local PTY         │             │ RDP  (IronRDP)     │
      │ SSH channel       │             │ VNC  (libvncclient)│
      │ telnet            │             │ X11 window         │
      │ serial            │             │                    │
      └───────────────────┘             └────────────────────┘
```

* [`Transport`](../crates/transport/src/lib.rs) — a bidirectional byte stream with a resizable text
  grid. Writes are synchronous and cheap; output arrives as `TransportEvent`s on a channel.
* [`GraphicalSurface`](../crates/surface/src/lib.rs) — a stream of pixel frames plus an input sink.

Both traits exist from the first commit even though only the local PTY implements one of them today.
The reason is scheduling: RDP and VNC land in phase 3, *before* SFTP. Had `Pane` been written
terminal-first, phase 3 would have begun with a rewrite of the pane, tab and layout code — the most
expensive kind of refactor, in the middle of the project. Defining both boundaries up front costs
almost nothing now and removes that risk entirely.

## Session is not tab

One SSH connection is one `Session`, and a session multiplexes channels:

```
Session (one TCP connection, one authentication)
├── terminal tab          (channel: session + shell)
├── SFTP browser panel    (channel: sftp subsystem)
├── port forward × N      (channels: direct-tcpip / forwarded-tcpip)
└── X11 forwarding        (channel: x11)
```

This is why BestTerm implements SSH in-process with `russh` rather than shelling out to `ssh` the way
XPipe does. Shelling out buys perfect OpenSSH compatibility for free, and BestTerm keeps that as an
*optional transport* for exotic authentication (GSSAPI/Kerberos, FIDO2 `sk-` keys, certificates,
corporate `ProxyCommand`). But it cannot bind an SFTP panel to the terminal's live session, which is
the interaction the product is built around.

## Threading

Three roles, and no others:

| Role | Owns | Never does |
|---|---|---|
| UI thread | `egui` frame loop, repaint-on-demand | blocking I/O, DNS, crypto |
| tokio runtime | all network and file I/O | touching UI state |
| helper processes | RDP and VNC protocol stacks | anything that can take the app down with it |

The UI and the runtime exchange state deltas over `crossbeam-channel`. The UI never awaits.

## Helper processes for frame protocols

RDP and VNC run in `bestterm-rdp` and `bestterm-vnc`, separate executables. Frames cross the boundary
through shared memory; input crosses through IPC. Three reasons, in order of importance:

1. **Crash isolation.** A malformed frame from an unknown server must not take down twenty open SSH
   sessions.
2. **Licence isolation.** `libvncclient` is GPL and links C code. Keeping it in its own process keeps
   that dependency out of the main binary's link graph.
3. **Replaceable backends.** If IronRDP does not cover multi-monitor or device redirection, swapping
   in FreeRDP is a change to one executable, invisible to the rest of the app.

This mirrors what `e-sh` does with its `e-sh-rdp` helper, which is prior art that this works.

## The terminal engine is behind a trait

[`TerminalEmulator`](../crates/core-terminal/src/lib.rs) wraps `alacritty_terminal`. The wrapper is
not ceremony:

* `libghostty-vt` has the best VT correctness available and already builds for Windows, Linux and
  Wasm behind a C API. It is a plausible future replacement, and the trait is what makes evaluating
  it a contained experiment.
* `alacritty_terminal`'s API is only partly public and is tuned for Alacritty's own needs. Depending
  on it directly from the UI would spread that coupling everywhere.

The trait's output is a `GridSnapshot` of plain `char` + RGB + flags. Colour resolution — named
colours, the 256-colour cube, dim/bold/inverse handling — happens in `core-terminal`, because it is
terminal *semantics*. `term-render` only ever sees resolved RGB, which makes it testable without a
VT parser.

## X11 is orchestrated, never implemented

```
proto-ssh opens the x11 channel
        │
        ▼
xserver crate ──► finds or launches an X server
        │           Linux:   the running Xorg/XWayland via $DISPLAY
        │           Windows: a bundled VcXsrv build (xorg-server is MIT)
        └──► allocates a display, generates the MIT-MAGIC-COOKIE, writes xauth
```

MobaXterm's own X server is a build of X.org. Writing one from scratch is a multi-year project in its
own right and is explicitly out of scope, permanently.

## Where the phase-0 shortcuts are

Marked here so nobody mistakes them for finished design:

* `term-render` currently paints the grid with `egui`'s text layout. The GPU glyph-atlas path
  (`swash` rasterisation → `etagere` atlas → `wgpu`, with damage tracking) is phase 1. The crate
  boundary is already correct; the implementation behind it is not yet.
* `ui-chrome` has the real chrome *structure* — menu bar, ribbon, sidebar with a vertical tab strip,
  tab bar, status bar — with placeholder actions. Pixel parity against
  [`ui-parity.md`](ui-parity.md) is phase 1.
* No configuration persistence yet. Phase 1.
