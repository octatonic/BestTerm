# Session dialog

Measured from MobaXterm Professional 26.4.0.5512 by opening the dialog and every protocol tab in it.
This is the largest single piece of interface work in the project and the one that decides whether
somebody moving across finds their settings where they expect them.

Names below are transcribed exactly, including the ones that look like mistakes: the group box for RDP
says **Basic Rdp settings**, not "Basic RDP settings", while the description line below says
`RDP (terminal services) session`. The inconsistent capitalisation is the reference's, and reproducing
the layout means reproducing it.

## Frame

Constant across every protocol:

```
┌────────────────────────────────────────────────────────────────────────┐
│ Session settings                                                    ✕  │
├────────────────────────────────────────────────────────────────────────┤
│  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐  … 15 tabs   │
│  │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│ │icon│               │
│  │SSH │ │Teln│ │Rsh │ │Xdmc│ │RDP │ │VNC │ │FTP │ │SFTP│               │
│  └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘ └────┘               │
├────────────────────────────────────────────────────────────────────────┤
│ ┌ Basic <Protocol> settings ────────────────────────────────────────┐  │
│ │  the fields that identify the target                              │  │
│ └───────────────────────────────────────────────────────────────────┘  │
│ ┌ Advanced … ┐┌ Terminal … ┐┌ Network … ┐┌ ★ Bookmark settings ┐       │
│ ├───────────────────────────────────────────────────────────────────┐  │
│ │                                                                   │  │
│ │            <description of the protocol>              ┌──────┐    │  │
│ │                                                       │ icon │    │  │
│ │                                                       └──────┘    │  │
│ └───────────────────────────────────────────────────────────────────┘  │
│                        ┌────────┐  ┌────────┐                          │
│                        │ ✓  OK  │  │ ✕ Cancel│                         │
│                        └────────┘  └────────┘                          │
└────────────────────────────────────────────────────────────────────────┘
```

Details that are easy to miss and cheap to get right:

* The dialog is **not** a separate window with its own title bar. It appears docked over the session
  area, its own title row reading `Session settings` with a close cross at the far right, and the
  application's ribbon still visible above it.
* The protocol tab strip is one row of fifteen tabs, icon above label, and it does not scroll or wrap
  at the sizes captured.
* Required fields are marked with a trailing asterisk in the *label*: `Remote host *`.
* `OK` and `Cancel` are centred, not right-aligned, and both carry a glyph — a green tick and a red
  cross.
* The description area holds one line of prose and a large icon in its lower right, except for WSL,
  which holds four lines.
* Each protocol's `Advanced` tab is named after the protocol: `Advanced SSH settings`,
  `Advanced Rdp settings`, `Advanced Sftp settings`.
* `Bookmark settings` carries a star glyph in its tab label. The others carry the protocol's icon.

## Protocols, in tab order

Fifteen, and the order is the reference's:

| # | Tab | Basic fields | Secondary tabs | Description line |
|---|---|---|---|---|
| 1 | SSH | `Remote host *`, `Username` (editable combo + pick-user button), `Port` = 22 | Advanced SSH settings · Terminal settings · Network settings · Bookmark settings | Secure Shell (SSH) session |
| 2 | Telnet | `Remote host *`, `Username` (plain field), `Port` = 23 | Advanced Telnet settings · Terminal settings · Network settings · Bookmark settings | Telnet session |
| 3 | Rsh | `Remote host *`, `Username` (plain field) — **no port** | Advanced Rsh settings · Terminal settings · Bookmark settings | RSH session |
| 4 | Xdmcp | radio `Connect to any server` (default) / `Specify server to connect to:` + field | Advanced Xdmcp settings · Bookmark settings | XDMCP (remote Unix desktop) session |
| 5 | RDP | `Remote host *`, `Username` (combo + pick-user button), `Port` = 3389 | Advanced Rdp settings · Network settings · Bookmark settings | RDP (terminal services) session |
| 6 | VNC | `Remote hostname or IP address *`, `Port` = 5900 — **no username** | Advanced Vnc settings · Network settings · Bookmark settings | VNC session |
| 7 | FTP | `Remote host *`, `Username` (combo + pick-user button), `Port` = 21 | Advanced Ftp settings · Bookmark settings | FTP session |
| 8 | SFTP | `Remote host *`, `Username` (combo + pick-user button), `Port` = 22 | Advanced Sftp settings · Bookmark settings | SFTP session |
| 9 | Serial | `Serial port *` (combo, default `Choose at session start`), `Speed (bps) *` (combo, default 9600) | Advanced Serial settings · Terminal settings · Bookmark settings | Serial (COM) session |
| 10 | File | `File/folder to open *` + browse-folder and browse-file buttons | Advanced File/folder settings · Bookmark settings | Launch a given URL, a local folder or a local file |
| 11 | Shell | `Terminal shell` (combo), `Startup directory` + browse button | Advanced Shell settings · Bookmark settings | Local shell session |
| 12 | Browser | `URL *` | Advanced Browser settings · Bookmark settings | Embedded internet browser |
| 13 | Mosh | `Remote host *`, `Username` (combo + pick-user button) — **no port** | Advanced Mosh settings · Terminal settings · Bookmark settings | Mosh (Mobile Shell) session |
| 14 | Aws S3 | `Key ID *` + info button, and the note `(Aws S3 sessions are experimental and should be used for test purpose only)` | Advanced Aws S3 (experimental) settings · Bookmark settings | Amazon Web Services S3 session |
| 15 | WSL | `Distribution` (combo, default `Default`), checkbox `Specify username` + field, `Run method` (combo, default `Autodetection`) | Advanced WSL settings · Terminal settings · Bookmark settings | see below |

Which secondary tabs a protocol gets is not arbitrary and is worth reading as a rule:

* **Terminal settings** appears only where there is a character stream to configure — SSH, Telnet,
  Rsh, Serial, Mosh, WSL. Not for RDP, VNC, Xdmcp, FTP, SFTP, File, Shell(!), Browser or Aws S3.
  `Shell` not having one is the surprise; it is worth confirming it is not simply a 26.4 regression.
* **Network settings** appears only where there is a socket to a named host that might need a proxy or
  a jump — SSH, Telnet, RDP, VNC. Not for FTP or SFTP, which is the second surprise.
* **Bookmark settings** appears on all fifteen.

### Shell: the terminal-shell list

The combo enumerates, in order:

1. `Bash (embedded)` — default
2. `Zsh (embedded)`
3. `Cmd`
4. `Windows PowerShell`
5. `PowerShell`
6. `Bash (external)`

Two of these have no counterpart here and one is a decision already taken: the embedded Bash and Zsh
are MobaXterm's bundled Cygwin environment, which `docs/ARCHITECTURE.md` lists as a permanent
non-goal. BestTerm's list is therefore shorter by design, and the slot is not reserved — unlike the
`Packages` ribbon button, a shell that cannot be launched is worse than absent.

`Windows PowerShell` and `PowerShell` are the two distinct products — the 5.1 that ships with Windows
and the cross-platform 7+. Both are detected by `crates/core-pty`, which is what the sidebar shows as
`Windows PowerShell` today.

### WSL: the description

Four lines rather than one, which is the only place the frame varies:

```
Windows Subsystem for Linux (WSL)

Run natively Linux distributions installed from the Windows store:
  - Take advantage of MobaXterm tabbed terminal
  - Run graphical applications thanks to MobaXterm X server
  - Use advanced terminal features (advanced copy/paste, logging, ...)
```

## Still to measure

Each protocol's **Advanced**, **Terminal**, **Network** and **Bookmark** tab contents. That is where
the "dozens of fields per protocol" live, and it needs the same treatment as the table above: label,
control type, default, validation, and the `.mxtsessions` key each maps to. The importer already knows
some of those keys — see `crates/importers/src/mxtsessions.rs` — so the mapping can be built from both
ends and checked in the middle.
