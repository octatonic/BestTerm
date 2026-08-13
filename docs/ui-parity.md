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

Measured from MobaXterm Professional 26.4.0.5512, maximised on a 3440x1440 display. The captured
window is 3456x1408 because a maximised window overhangs the screen by the invisible 8-pixel resize
border on every side, so every figure below has had that border removed.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ title bar                                                            23 px   │
├──────────────────────────────────────────────────────────────────────────────┤
│ Terminal  Sessions  View  X server  Tools  Settings  Macros  Help            │  menu bar
├──────────────────────────────────────────────────────────────────────────────┤
│  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐        X  ⏻ │
│  │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│  ...        │  ribbon
│  │Sess│ │Serv│ │Tool│ │Sess│ │View│ │Spli│ │Mult│ │Tunn│ │Pack│    no labels │
│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘             │
├──────────────────────────────────────────────────────────────────────────────┤
│ [ Quick connect...        ] ╱🏠╲╱ + ╲                                        │  one row:
│                              ‾‾‾  ‾‾‾                                        │  field + tabs
├───┬──────────────────────────────────────────────────────────────────────────┤
│ ★ │                                                                      ▲   │
│ ⇱ │  terminal grid                                                           │
│ ⇲ │                                                                          │
│35 │  ~300 px session tree                                                    │
├───┴──────────────────────────────────────────────────────────────────────────┤
│ status bar — could not be measured from a maximised capture (see below)      │
└──────────────────────────────────────────────────────────────────────────────┘
```

Measured bands, from a colour scan down a column clear of icons and text at x=1500:

| Band | Extent (window pixels, border removed) | Height | Fill |
|---|---|---|---|
| Title bar | 0..22 | 23 | `#211E26` |
| Menu bar + ribbon + quick-connect row | 23..121 | 99 | `#141414`, no separators between them |
| Separator | 122 | 1 | `#6A6A6A` |
| Content | 123.. | rest | `#202020` |

And across, at mid-height:

| Band | Extent | Width | Fill |
|---|---|---|---|
| Vertical icon strip | 0..34 | **35** | `#141414` |
| Separator | 35 | 1 | `#6A6A6A` |
| Session tree panel | 36..334 | **~299** | `#141414`, scrollbar at its right edge |
| Separator | 335 | 1 | `#6A6A6A` |
| Terminal area | 336.. | rest | `#202020` |

Three findings from the capture that contradict what this document assumed:

* **Quick connect and the tab bar share one row.** The field sits on the left at roughly 333 px wide,
  and the tab bar begins immediately to its right, starting with a home tab and a `+` tab. There is
  **no Go button** — the field is committed with Enter. An earlier draft gave quick connect a
  full-width row of its own and a Go button beside it.
* **The menu bar, ribbon and quick-connect row are one continuous fill** with no rules between them.
  Only the boundary with the content area is drawn, as a single grey hairline.
* **The left edge strip is 35 px of icons, not rotated text.** This installation shows a star and two
  arrows rather than the words `Sessions` / `Tools` / `Macros` / `Sftp`. Whether that is a 26.4 change
  or a setting is `MEASURE` — it decides whether the rotated labels, described below as the most
  distinctive element of the layout, are still part of the target at all.

Not measurable from this capture, and needing a second one at a non-maximised size:

* The status bar. A maximised window puts its last eight pixels past the bottom of the screen, which
  is exactly where the status bar is, so nothing about its height or contents can be read from here.
* Anything about resize behaviour, the collapsed sidebar, or the pin / unpin / auto-hide states.

The theme in the captured installation is **dark**. This document and the implementation both assumed
the light theme; which one is the default for a fresh install, and therefore which one parity is judged
against, is `MEASURE`.

## Element inventory

### Menu bar

Measured from a capture of MobaXterm Professional 26.4.0.5512. **Eight** menus, in this order:

| # | Menu | Items |
|---|---|---|
| 1 | Terminal | `MEASURE` — enumerate by opening each menu |
| 2 | Sessions | `MEASURE` |
| 3 | View | `MEASURE` |
| 4 | X server | `MEASURE` |
| 5 | Tools | `MEASURE` |
| 6 | Settings | `MEASURE` |
| 7 | Macros | `MEASURE` |
| 8 | Help | `MEASURE` |

Ordering above is the target ordering and must not be "improved".

An earlier version of this document listed ten menus — Terminal, Sessions, View, Split, MultiExec,
Tunneling, Packages, Settings, Macros, Help — and asserted that the titles "are already correct".
They were not: they were written from memory before anyone had looked. `Split`, `MultiExec`,
`Tunneling` and `Packages` exist only on the ribbon, and the two menus that *are* there, `X server`
and `Tools`, were missing. The lesson is the one this document exists for: a claim about the reference
is worth nothing until it has been measured against the reference.

### Ribbon toolbar

A single row of large buttons, each an icon above a text label.

Measured from 26.4.0.5512. Two groups, and the split between them matters: eleven buttons drawn
left-to-right with an icon above a text label, then a gap, then two buttons pinned to the right edge
that have **an icon and no label at all**.

| # | Button | Label? | Action | Notes |
|---|---|---|---|---|
| 1 | Session | yes | open the Session dialog | primary entry point |
| 2 | Servers | yes | server tools submenu | phase 7 |
| 3 | Tools | yes | local tools submenu | phase 7 |
| 4 | Sessions | yes | session list / switcher | |
| 5 | View | yes | layout controls | |
| 6 | Split | yes | pane splitting | |
| 7 | MultiExec | yes | broadcast input to panes | phase 8 |
| 8 | Tunneling | yes | tunnel manager | phase 2 |
| 9 | Packages | yes | package manager | out of scope, see ARCHITECTURE.md on not cloning Cygwin |
| 10 | Settings | yes | settings dialog | |
| 11 | Help | yes | help / about | |
| — | *right edge* | | | |
| 12 | X server | **no** | toggle + status | phase 6 |
| 13 | Exit | **no** | quit | red power glyph |

One deliberate divergence, and one correction:

* **Games** — earlier drafts of this document recorded a `Games` button as present in the original and
  deliberately omitted here. 26.4 has no such button, so there is nothing to omit; the note is kept
  only so the disappearance is not mistaken for an oversight later.
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
