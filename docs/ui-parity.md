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
* **The left edge strip is icons, not rotated text — and there are three of them, not four.** Magnified,
  the strip holds a gold star (Sessions), a red folding knife (Tools) and a blue paper plane (Sftp),
  each in a square button, with no text anywhere. The active one is drawn without a border while the
  others have one. `Macros`, which this project's strip has as a fourth tab, is not on the strip at all.

  This is the finding with the widest consequences, because the rotated labels were described here as
  the most distinctive element of the whole layout and the thing imitators get wrong. They are not in
  26.4. What this project reproduced faithfully was an **older** MobaXterm.

  The target is therefore the icon strip. It is not being changed yet, and deliberately: the icon set
  does not exist, and replacing the only labels in the strip with three empty squares would trade a
  wrong-but-usable strip for a right-but-unusable one. The rotated text stays as a stand-in until there
  are icons to put in its place, and `crates/ui-chrome` says so where the strip is drawn.

### The second capture: windowed, 1400x900

Taken to reach what a maximised window hides. It confirmed the bands above to the pixel — 30 px of title
bar, then the same **99 px** of menu bar, ribbon and quick-connect row, then the same single grey
hairline — which is worth more than it sounds: the two captures were measured independently and agree,
so 99 px is a real figure and not an artefact of how one screenshot was taken.

And it produced one finding that changes the implementation:

* **There is no status bar.** The session tree runs to the bottom edge of the window, and below it is a
  one-pixel border and nothing else. This project draws a status bar carrying the X server state, the
  grid size and the session description.

  The honest limit of this observation: it says *this installation* shows none. MobaXterm has had a
  status bar historically and `View` almost certainly still has a toggle for it, so whether 26.4 removed
  it or this configuration hid it is `MEASURE`, and the answer decides whether ours should be there at
  all or merely be switchable off.

Still not measured:

* The items inside each of the eight menus. This needs the menus opened one at a time, which is the one
  part of this that a person does far faster than a script: open each, read the items, paste them in.
* Resize behaviour, the collapsed sidebar, and the pin / unpin / auto-hide states.
* Behaviour at 150% and 200% display scaling.

The theme in the captured installation is **dark**, and that is a setting rather than the default:
`MobaXterm.ini` carries `DarknessIntensity=80` and `SkinSat=80`, which are the controls for it. Parity is
therefore judged against the light chrome, which is what this project already implements, and dark
becomes a theme to support rather than the target to match. The structure measured above is the same
under either.

One cross-check worth recording, because it is the only independent confirmation of any figure here:
the same file carries `SidebarWidth=336`, and the colour scan measured 35 px of strip, a 1 px
separator, 299 px of tree and another 1 px separator — 336 exactly.

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

Measured. Fifteen protocol tabs, their basic fields, their secondary tabs and their description lines
are in [`ui-parity/session-dialog.md`](ui-parity/session-dialog.md). What remains is the contents of
each protocol's Advanced / Terminal / Network / Bookmark tab, which is where the "dozens of fields"
live.

Three things the measurement settled that guesswork had wrong:

* Fifteen protocols, not the eleven this document listed. `Rsh`, `Xdmcp`, `File` and `Aws S3` were
  missing from it entirely.
* Which secondary tabs a protocol gets follows a rule rather than being uniform: `Terminal settings`
  only where there is a character stream, `Network settings` only for SSH, Telnet, RDP and VNC.
* `Shell` has no `Terminal settings` tab and `SFTP` has no `Network settings` tab. Both look like
  oversights in the reference and both are reproduced until shown otherwise.

### Sidebar panels

Measured. Three panels, their icons and their full contents are in
[`ui-parity/sidebar-panels.md`](ui-parity/sidebar-panels.md).

The correction that mattered: there are **three**, not four. This project had `Sftp` as a fourth,
because the older strip did; the file browser docks inside a session tab instead. And the Tools panel's
nineteen entries are now enumerated, which is the authoritative scope for the tool set that
`ROADMAP.md` phase 7 exists to build.

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

## Capturing a state without touching the mouse

Parity is judged by comparing screenshots, and a screenshot of the session dialog needs the session
dialog open. Driving synthetic clicks at a desktop to get there is unreliable — Windows refuses
`SetForegroundWindow` from a background process, so a capture can silently end up showing whatever
window was underneath — and it is rude to somebody who is using the machine.

So the state is nameable:

```sh
BESTTERM_UI_STATE=session-dialog bestterm
BESTTERM_UI_STATE=tools bestterm
BESTTERM_UI_STATE=macros bestterm
```

Captures are taken with `PrintWindow` and the `PW_RENDERFULLCONTENT` flag, which renders the window
itself rather than reading the screen, so an overlapping window cannot contaminate the result. Both
halves of that are worth keeping: the first capture attempt in this project produced a screenshot of an
unrelated chat application and very nearly got reported as evidence.

## Automated enforcement

`ui-chrome` carries screenshot tests that render each chrome element at fixed sizes and compare
against committed reference PNGs of **BestTerm's own** output. They catch regressions in our chrome;
they do not compare against MobaXterm. Parity against the reference application is verified by the
human checklist above, against the local captures.
