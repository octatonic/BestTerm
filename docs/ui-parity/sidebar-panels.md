# Sidebar panels

Measured from MobaXterm Professional 26.4.0.5512 by opening each panel on the left edge strip.

## There are three, not four

The strip carries three icon-only buttons, top to bottom:

| # | Icon | Panel | Tooltip |
|---|---|---|---|
| 1 | gold star | Sessions | `Sessions` |
| 2 | red folding knife | Tools | |
| 3 | blue paper plane | Macros | |

`Sftp` is **not** one of them. An earlier reading of the capture guessed the paper plane was Sftp
because the older MobaXterm strip had a `Sftp` tab; opening it shows Macros. The file browser is not a
sidebar panel at all — it docks inside a session tab, which is what `docs/ROADMAP.md` phase 4 already
assumed.

`crates/ui-chrome` had four panels, `Sessions / Tools / Macros / Sftp`, and now has three.

The active button is drawn without a border; the inactive ones have one. Hovering shows a tooltip with
the panel name — which is how a strip with no text stays usable, and is worth copying.

## Sessions

A tree of folders and sessions, with a disclosure arrow and a folder icon per folder. Nothing else on
the panel: no header row, no search field, no toolbar. The tree begins immediately below the strip's
top edge.

## Tools

A flat list under three **category headers**, drawn as full-width bars with centred text. Every entry
is an icon and a label. Transcribed exactly:

**System**

* MobApt packages manager (experimental)
* X11 tab with Jwm
* X11 window with Jwm
* X11 window with Twm
* List hardware devices
* List running processes
* Command Prompt (admin)
* Windows Powershell (admin)

**Office**

* MobaTextEditor
* MobaDiff
* Ascii table

**Network**

* Network services
* MobaSSHTunnel (port forwarding)
* MobaKeyGen (SSH key generator)
* List open network ports
* Wake On Lan
* Network scanner
* Ports scanner
* Network packets capture

This list is the authoritative scope for the tool set, and it settles several things the roadmap had
been guessing at:

* The SSH key generator and the tunnel manager are **here**, in this panel, not only behind ribbon
  buttons. `MobaSSHTunnel` is the graphical tunnel manager that `ROADMAP.md` phase 2 calls for, and
  `MobaKeyGen` is the key generator phase 7 calls for.
* `Network packets capture` is on the list. Nothing in the roadmap covers packet capture, and it is a
  considerably larger undertaking than the other entries — it needs a capture driver on Windows and
  elevated privileges on both platforms.
* `MobApt packages manager` is present and is a permanent non-goal here: it manages the bundled Cygwin
  environment. Its slot is not reserved.
* The three X11 entries — a tab with Jwm, a window with Jwm, a window with Twm — are concrete X server
  launch modes and belong with phase 6 rather than with the tools.
* `Command Prompt (admin)` and `Windows Powershell (admin)` are elevated local shells, which is a
  small addition to `crates/core-pty` rather than a tool of its own.

## Macros

Two entries and no list of anything else when nothing is recorded:

* `Record new macro`, with a red record glyph, and a hamburger menu at the right of the row
* `Saved macros`, with a person-and-window glyph

The hamburger on the first row implies a menu of macro-management actions; its contents are `MEASURE`.
