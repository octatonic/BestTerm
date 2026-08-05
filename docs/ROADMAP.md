# Roadmap

Each phase is shippable on its own. The ordering is deliberate: the two protocol abstractions land
before the volume of protocol code that would otherwise force a mid-project rewrite, and the
`.mxtsessions` importer ships with the public beta because it is the cheapest way to let people bring
their existing configuration across.

An honest note on scale: MobaXterm is roughly fifteen years of work by a commercial team. Full
feature parity is a multi-year programme, not a quarter. Nothing below claims otherwise.

## Phase 0 — skeleton

Workspace and crate boundaries · CI on Windows and Linux from the first commit · GPL-3.0 licensing ·
`core-pty` + `core-terminal` + `term-render` · one tab running a local shell ·
[`docs/ui-parity.md`](ui-parity.md).

## Phase 1 — GUI and terminal

The chrome from `ui-parity.md`: menu bar, ribbon, quick connect, left dock panel with a rotated edge
tab strip, tab bar, status bar, theme, own icon set with the `ImgNum` mapping. GPU glyph-atlas
rendering (`swash` → `etagere` → `wgpu`) with damage tracking, ligatures, colour emoji, TrueColor,
scrollback and search. Splits (2/3/4). TOML configuration with schema versioning and layout
restoration.

**Gate:** a side-by-side screenshot matches on structure, and rendering feels fast on both OSes.

## Phase 2 — SSH

`transport` + `proto-ssh` on `russh`: password, public key, ssh-agent, keyboard-interactive · jump
chains · `~/.ssh/config` · `known_hosts` with a fingerprint confirmation UI · keepalive and
reconnect · the external-OpenSSH transport adapter for GSSAPI, FIDO2 and certificates · session tree
with folder setting inheritance · the vault (Argon2id + XChaCha20-Poly1305, master key in the OS
keyring) · local/remote/dynamic forwards with a graphical manager · the SSH session dialog.

## Phase 3 — RDP and VNC → public beta 0.9

`surface` + `ipc-frame` (shared-memory frame transport) · `bestterm-rdp` on IronRDP with clipboard,
dynamic resize, multi-monitor, NLA/CredSSP and RemoteFX · `bestterm-vnc` on `libvncclient` with
Tight/ZRLE/Hextile/Raw and cursor pseudo-encodings · RDP and VNC session dialogs · importers for
`.mxtsessions`, PuTTY's registry, `ssh_config` and WinSCP · packaging and Windows code signing.

## Phase 4 — SFTP → 1.0

`proto-sftp` · dual-pane browser bound to the live SSH session · follow-terminal-directory · transfer
queue with resume · remote file editing · permissions and ownership.

A note on ordering: a file browser is a first-order feature for this class of tool and is
considerably cheaper than RDP plus VNC. RDP/VNC come first by explicit decision; 1.0 is nonetheless
only declared once SFTP is in, with beta 0.9 shipping after phase 3 so there is no long wait.

## Phase 5 — the simple protocols

Telnet, serial, rlogin, FTP, SPICE. Also the test of whether `Transport` leaks.

## Phase 6 — X11

The SSH x11 channel, xauth and display allocation · orchestration of the running Xorg/XWayland on
Linux · a bundled VcXsrv build on Windows with lifecycle management · XDMCP last. The most expensive
phase in the plan; scoped as orchestration of an existing X server, never as writing one.

## Phase 7 — power features

Multi-exec broadcast · macros · session logging and recording · tmux control mode · zmodem/trzsz ·
command palette and history-based completion.

## Phase 8 — extensibility → 2.0

`plugin-host` on `wasmtime` with a capability model, SDK and scaffolding · configuration sync over
Git · team vaults.

## Permanent non-goals

* Cloning Cygwin / `MobApt`. WSL and PowerShell detection instead.
* Writing an X server, an RDP stack or a VNC stack from scratch.
* Windows 7 and niche distribution support at launch.
* Using MobaXterm's icons, artwork or name.
