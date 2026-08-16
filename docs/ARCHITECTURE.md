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

### A fourth reason arrived uninvited: separate dependency graphs

The RDP helper lives in a cargo workspace of its own, at `helpers/rdp`. Not by preference —
`ironrdp-connector` pins `picky = "=7.0.0-rc.25"`, which pins `ecdsa = "=0.17.0-rc.22"`, while `russh`
requires `ecdsa = "^0.17"`, and a caret requirement does not match a pre-release. Both fall in the same
semver compatibility range, so one graph must choose one of them, and no feature on either side removes
the dependency. SSH and RDP therefore cannot share a dependency graph until IronRDP unpins picky.

The split is the boundary above applied one level down, and it costs two things worth knowing:

* Every cargo command has to name the workspace: `--manifest-path helpers/rdp/Cargo.toml`. CI runs
  each of fmt, clippy, test, build, doc and cargo-deny twice for this reason.
* `ipc-frame`, `surface` and `core-vault` are referenced by path and compiled twice. None of them
  depends on russh or picky, so there is nothing to conflict over.

VNC does not need the same treatment unless its backend brings a conflicting dependency of its own.

The split has an expiry date, and it is visible upstream: `picky` 7.0.0-rc.26 dropped its `ecdsa`
dependency entirely. The moment an `ironrdp-connector` release pins that or later, the conflict is
gone and the two workspaces can be merged back into one. Worth re-checking whenever IronRDP
publishes — `helpers/rdp` is a workaround with a known end, not a permanent boundary.

### Trust decisions are duplicated on purpose, for now

`proto-ssh::known_hosts` and `proto-rdp::server_key` have the same shape — the same four verdicts, the
same decision type, the same "revoked is decided before anyone is asked" ordering — and share no code.
The file formats have nothing in common (one is OpenSSH's, with globs and hashed hostnames), the two
crates are in different workspaces, and two similar things are not yet a pattern. When VNC needs a
third, the common part is worth extracting into a crate that depends on neither protocol.

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

## The vault uses two keys, not one

```
master password ──Argon2id(salt)──► KEK ──seals──► DEK ──seals──► every entry
                                     │              │
                         stored: salt + costs   stored: wrapped blob
```

The data-encryption key is random and never leaves the vault. The key-encryption key is derived from
the master password and does nothing but unwrap it. Three properties follow, none of which work with
a single derived key:

* Changing the master password rewraps one blob instead of re-encrypting every secret, so the file's
  diff is two lines.
* The OS keystore can hold the DEK for a password-free unlock without ever holding the password.
* Argon2 costs live in the file, so raising them later does not lock anyone out of an existing vault.

Every entry is authenticated against **its own name**. Without that, someone with write access to
the file could move the ciphertext of `staging/password` onto `production/password` and every
integrity check would still pass.

The session tree references credentials by opaque handle and holds none of them. That separation is
what lets `sessions.toml` be a readable, git-synchronisable file — and it is the specific thing
MobaXterm's `.mxtsessions` format gets wrong, storing SFTP passwords in clear text.

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
* Both lockfiles are committed now, which they were not for a long time — CI re-resolved the graph
  on every run, and that is how a vulnerable `time` was selected once already. Kept in the list
  because the *reason* still applies: a green run has to pin what a later run will build.
* `rust-version = "1.95"` is load-bearing, not decorative. cargo resolves to the newest versions the
  declared MSRV permits, so understating it makes cargo prefer *older* dependencies — including ones
  with unfixed advisories. The number must track what the tree actually requires.
* The application has now been run, and the first two things it did were expose bugs no test could
  have: output arriving did not wake the interface, and a placeholder icon that was the protocol's
  first letter produced tabs labelled `sC:\Windows\...`. Both are fixed. What is still unverified is
  everything measured rather than merely present: pixel parity against `ui-parity.md`, behaviour at
  150% and 200% scaling, and font metrics under a different DPI.
* **RDP has never been run against a real server.** Every part of it exists — the handshake, the
  active stage, the helper process, the boundary, a pane that shows the frame, keyboard and mouse —
  and the only thing tested end to end is the process boundary, against a closed port. Nobody has
  watched a desktop appear. Until somebody has, treat the parts as separately correct and jointly
  unproven; the likely failure is not in any one of them but in what they assume about each other.
* The RDP helper cannot send composed text or share the clipboard. Both are reported once per kind
  rather than dropped quietly, so the gap is visible where it happens.
* Keys arrive from egui, which has no scan code — only a key identity — so the mapping in
  `app-ui/src/keymap.rs` covers the keys egui can name and no others. Caps Lock, Num Lock, Print
  Screen, Pause, the context-menu key and the whole numeric keypad have no egui variant and therefore
  cannot be forwarded at all.
* The vault has no OS keystore backend, so the master password is typed once per run of the
  application rather than being unlocked from DPAPI or the Secret Service.
