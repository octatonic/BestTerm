# UI parity specification

BestTerm reproduces MobaXterm's layout, information architecture and interaction model. This document
is the **single source of truth** for that work: `ui-chrome` and `app-ui` are built from this file,
and the acceptance checklist at the bottom is what a release is signed off against.

## Scope and limits

**In scope:** window layout, panel structure, control placement, sizing behaviour on resize, menu and
context-menu contents, keyboard shortcuts, tab behaviour, the information architecture of dialogs.
Layout and interaction patterns are not protectable, and reproducing them is the point of the
project.

**Out of scope, permanently:** MobaXterm's icons, artwork, bitmaps, cursors, sounds, wording of its
help text, and its name. These are copyrighted or trademarked. BestTerm ships its own icon set from a
permissively licensed family (Papirus / Tabler / Lucide) and its own strings.

`ImgNum` values in imported `.mxtsessions` files are mapped to BestTerm icons through
[`crates/importers`](../crates/importers) so that imported trees keep their visual distinctions
without reusing any original artwork. That mapping table lives with the importer, not here.

## How to fill in the measurements

Sections below marked `MEASURE` are placeholders. Do not guess them, and do not implement against a
guess — a chrome built from approximate numbers looks subtly wrong in a way that is expensive to
correct later.

Procedure, run once on Windows at 100% display scaling with the default MobaXterm theme:

1. Capture the main window, maximised, with: one SSH tab connected, the Sessions sidebar open and
   pinned, and an SFTP panel visible. Save to `docs/ui-parity/captures/` (git-ignored — these are
   local reference material, not redistributable assets).
2. Capture each of: the Session dialog on every protocol tab, every top-level menu opened, the tab
   context menu, the session-tree context menu, the Settings dialog on every tab.
3. Measure from the captures with a pixel ruler and record values in the tables below, in logical
   pixels at 100% scaling. Record the *rule*, not just the number, wherever a size is derived
   (e.g. "sidebar width is user-draggable, default N, minimum M").
4. Repeat step 1 at 150% and 200% scaling and record only what changes *non-proportionally*.

Until a row has real numbers, `ui-chrome` uses a clearly-labelled provisional constant so that the
gap is visible in code review rather than hidden.

## Window structure

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ title bar                                                                    │
├──────────────────────────────────────────────────────────────────────────────┤
│ Terminal  Sessions  View  Split  MultiExec  Tunneling  Packages  Settings ...│  menu bar
├──────────────────────────────────────────────────────────────────────────────┤
│  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐      │
│  │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│  ...  │  ribbon
│  │Sess│ │Serv│ │Tool│ │Game│ │Sess│ │View│ │Spli│ │Mult│ │Tunn│ │Pack│      │
│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘      │
├──────────────────────────────────────────────────────────────────────────────┤
│ Quick connect: [ user@host:port                                    ] [ Go ]  │
├───┬──────────────────────────────────────────────────────────────────────────┤
│ S │ ┌──────────┬──────────┬────────────┐                                  ▲  │
│ e │ │ 1. host  │ 2. host  │     +      │                                     │  tab bar
│ s │ ├──────────┴──────────┴────────────┴──────────────────────────────────┐  │
│ s │ │                                                                      │  │
│ i │ │  terminal grid                                                       │  │
│ o │ │                                                                      │  │
│ n │ │                                                                      │  │
│ s │ └──────────────────────────────────────────────────────────────────────┘  │
│ ─ │                                                                          │
│ T │  (for SSH sessions an SFTP panel docks inside the tab — phase 4)         │
│ o │                                                                          │
│ o │                                                                          │
│ l │                                                                          │
│ s │                                                                          │
├───┴──────────────────────────────────────────────────────────────────────────┤
│ X server: running  ·  DISPLAY=:0  ·  80x24  ·  ssh user@host                 │  status bar
└──────────────────────────────────────────────────────────────────────────────┘
```

The left edge strip carries **vertically rotated** tab labels. This is the most distinctive element
of the layout and the one most often got wrong by imitators: the labels rotate, the strip is always
visible even when the panel is collapsed, and the panel has pin / unpin / auto-hide states.

## Element inventory

### Menu bar

| Menu | Status | Items |
|---|---|---|
| Terminal | confirmed present | `MEASURE` — enumerate from capture |
| Sessions | confirmed present | `MEASURE` |
| View | confirmed present | `MEASURE` |
| Split | confirmed present | grid layouts (2 / 3 / 4 panes) — enumerate exact set |
| MultiExec | confirmed present | `MEASURE` |
| Tunneling | confirmed present | `MEASURE` |
| Packages | confirmed present | `MEASURE` |
| Settings | confirmed present | `MEASURE` |
| Macros | confirmed present | `MEASURE` |
| Help | confirmed present | `MEASURE` |

Ordering above is the target ordering and must not be "improved".

### Ribbon toolbar

A single row of large buttons, each an icon above a text label.

| Button | Action | Notes |
|---|---|---|
| Session | open the Session dialog | primary entry point |
| Servers | server tools submenu | phase 7 |
| Tools | local tools submenu | phase 7 |
| Games | — | present in the original; BestTerm omits it deliberately, see note below |
| Sessions | session list / switcher | |
| View | layout controls | |
| Split | pane splitting | |
| MultiExec | broadcast input to panes | phase 8 |
| Tunneling | tunnel manager | phase 2 |
| Packages | package manager | out of scope, see ARCHITECTURE.md on not cloning Cygwin |
| Settings | settings dialog | |
| Help | help / about | |
| X server | toggle + status | phase 6 |
| Exit | quit | |

Two deliberate divergences, both recorded here so they are decisions rather than omissions:

* **Games** — a novelty in the original with no role in a remote-access tool. Not reproduced.
* **Packages** — MobaXterm's `MobApt` manages its bundled Cygwin environment. BestTerm does not
  bundle Cygwin (it detects WSL and PowerShell instead), so the button has nothing to manage. The
  slot is reserved rather than reused, to keep the ribbon's shape recognisable.

| Metric | Value |
|---|---|
| button width / height | `MEASURE` |
| icon size | `MEASURE` |
| label font and size | `MEASURE` |
| spacing between buttons, group separators | `MEASURE` |
| behaviour when the window is too narrow | `MEASURE` — overflow chevron? clipping? |

### Left dock panel

| Property | Value |
|---|---|
| edge tab labels | Sessions, Tools, Macros, Sftp — confirm the full set and order from capture |
| label orientation | rotated 90°, reading bottom-to-top — confirm direction from capture |
| default / min / max width | `MEASURE` |
| pin, unpin, auto-hide affordances | `MEASURE` |
| strip visible when collapsed | yes |

Sessions panel contents:

| Property | Value |
|---|---|
| tree root label | "User sessions" |
| node kinds | folder, session; per-protocol icons |
| toolbar above the tree | `MEASURE` — new session, new folder, search, … |
| context menu | `MEASURE` — enumerate every item |
| drag & drop | reorder and reparent within the tree |
| inline rename | `MEASURE` — F2? double-click? |

### Tab bar

| Property | Value |
|---|---|
| tab contents | protocol icon, session name, close button |
| `+` button | opens a new local terminal tab |
| overflow | `MEASURE` — scroll arrows or dropdown |
| context menu | rename, duplicate, colour, detach — confirm full set |
| tab colour | per-session, imported from `.mxtsessions` |
| middle-click | `MEASURE` — closes? |

### Session area

| Property | Value |
|---|---|
| split layouts | 2 / 3 / 4 panes — record exact geometries offered |
| pane focus indication | `MEASURE` |
| SFTP panel position and default width | `MEASURE` (phase 4) |
| "follow terminal directory" behaviour | tracks the shell's cwd — confirm mechanism (OSC 7?) |

### Status bar

| Segment | Content |
|---|---|
| X server state | running / stopped, plus `DISPLAY` |
| terminal size | `cols x rows`, live during resize |
| session info | protocol and target of the focused pane |
| remaining segments | `MEASURE` |

### Session dialog

The largest single piece of UI work in the project: a tabbed dialog per protocol with dozens of
fields each. Enumerate exhaustively per protocol — SSH, RDP, VNC, SFTP, FTP, Telnet, Serial, Shell,
Browser, Mosh, WSL — recording for every field: label, control type, default, validation, tooltip,
and the `.mxtsessions` key it maps to.

Track it in a companion file per protocol (`docs/ui-parity/session-dialog-ssh.md`, …) rather than
inflating this document.

### Theme

| Property | Value |
|---|---|
| base palette | dense light, grey-blue accents — sample exact values from capture |
| UI font and size | `MEASURE` |
| control heights, border widths, corner radii | `MEASURE` (the original is square-cornered and thin-bordered) |
| terminal default palette | `MEASURE` — 16 ANSI colours + default fg/bg |

Implemented by replacing `egui::Style` and `egui::Visuals` wholesale plus custom widgets — not by
tweaking egui's defaults, which are rounded and airy and will never converge on this look.

## Acceptance checklist

Signed off per release. A row passes only when a side-by-side capture at the same window size shows
no structural difference.

- [ ] Menu bar: same menus, same order, same items, same accelerators
- [ ] Ribbon: same buttons in the same order, same metrics, same overflow behaviour
- [ ] Quick connect bar: same position, parses `user@host:port`, history autocomplete
- [ ] Left panel: rotated edge labels, same set and order, pin/unpin/auto-hide, strip visible when collapsed
- [ ] Session tree: same root label, node kinds, toolbar, context menu, drag & drop, inline rename
- [ ] Tab bar: same tab anatomy, `+` behaviour, overflow, context menu, colours
- [ ] Splits: same layouts offered, same focus indication
- [ ] Status bar: same segments in the same order, live updates
- [ ] Session dialog: every field present per protocol, same grouping across tabs
- [ ] Theme: fonts, metrics and palette within tolerance of the sampled values
- [ ] 150% and 200% scaling: no clipped labels, no overlapping controls
- [ ] Keyboard: every shortcut in the reference does the same thing here
- [ ] Screenshot diff tests green (`cargo test -p bestterm-ui-chrome`)

## Automated enforcement

`ui-chrome` carries screenshot tests that render each chrome element at fixed sizes and compare
against committed reference PNGs of **BestTerm's own** output. They catch regressions in our chrome;
they do not compare against MobaXterm. Parity against the reference application is verified by the
human checklist above, against the local captures.
