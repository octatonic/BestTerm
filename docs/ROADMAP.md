# Roadmap

Each phase is shippable on its own. The ordering is deliberate: the two protocol abstractions land
before the volume of protocol code that would otherwise force a mid-project rewrite, and the
`.mxtsessions` importer ships with the public beta because it is the cheapest way to let people bring
their existing configuration across.

An honest note on scale: MobaXterm is roughly fifteen years of work by a commercial team. Full
feature parity is a multi-year programme, not a quarter. Nothing below claims otherwise.

## Where things actually stand

The phases below describe intent. This section describes the repository, because the two had drifted
and nothing was recording the difference.

**Every crate is now in an executable.** That sentence was false for most of the project's life and
was corrected twice after being claimed too early, so it is worth being precise about: `apps/bestterm`
reaches `app-ui`, `ui-chrome`, `core-terminal`, `core-pty`, `term-render`, `transport`, `proto-ssh`,
`core-model`, `config`, `core-vault` and `importers`; `helpers/rdp/apps/bestterm-rdp` reaches
`proto-rdp`, `ipc-frame` and `surface`, which were the last two nothing linked. There is no longer a
crate that is tested and unreachable.

**The rhythm changed when there was a local toolchain.** For a long stretch CI was the only compiler
and every mistake cost a round trip; a full build is now 36 seconds and an incremental one about five,
which is why the last stretch of work is wiring rather than more libraries.

Phase 2, honestly:

| Phase 2 item | State |
|---|---|
| Authentication, jump chains, `ssh_config`, `known_hosts` | done, verified against a real `sshd` |
| `known_hosts` fingerprint confirmation UI | done |
| Session tree with inheritance | done, and reachable: the sidebar opens saved sessions |
| Vault | reachable; no OS keystore backend yet, so the master password is typed each session |
| Local, remote and dynamic forwards | done, with a graphical manager |
| Keepalive and death detection | done: 30s/3, and the reason a session ended reaches the tab |
| Reconnect | user-initiated, with the host key pinned. Automatic is deliberately not built — see below |
| External-OpenSSH transport adapter | absent |
| SSH session dialog | the dialog exists with all 15 protocol tabs; the per-protocol field sets are measured for Basic only |

Phase 3: RDP is built end to end — configuration, server-key pinning, the handshake, the active
stage, the helper process, the process boundary, a pane that shows the frame, keyboard and mouse. It
has never been run against a real RDP server, and that sentence is the important one: the parts are
separately tested and jointly unproven, and the likely failure is in what they assume about each
other rather than in any one of them. VNC has not started, though `helper-surface` is written so that
its helper is a parameter rather than a special case.

**Reconnect, and why only half of it exists.** A reconnect re-authenticates to a host named by a
*string*, and that name is resolved afresh every time. Between the first connection and the reconnect,
DNS, `/etc/hosts`, DHCP or a VPN can point it at a different machine — and a reconnect that then
offered the password or private key to whatever answered, with nobody reading a fingerprint, would
hand the credential to the wrong host while looking exactly like a network hiccup recovering.

Re-running the `known_hosts` policy does not catch that, and is the wrong question besides: it asks
whether the *address* is trusted, and the address is what moved. It would also re-raise the prompt,
because a key accepted by prompt during this session is not in the snapshot the connection was
verifying against — and training somebody to click through a host key dialog on every network blip is
the precise failure host key checking exists to prevent.

So [`crates/proto-ssh/src/reconnect.rs`](../crates/proto-ssh/src/reconnect.rs) pins: it compares
against the key the dying connection actually saw, and a mismatch is fatal rather than a question. A
session that cannot be reopened — a one-time code, which cannot be replayed — says so when it opens
rather than failing later for a reason nobody would trace back.

What exists is the user-initiated half: a dead session offers `Reconnect`, and clicking it opens a
fresh one beside the old tab. The old tab stays, because `russh` has no resumption — the working
directory, the history, whatever was running and the scrollback are gone — and a terminal that came
back empty in the same tab would look like it lost its contents to a bug.

Automatic reconnect is not built, and `reconnect::should_retry` is the decision it will spend: a
server-sent disconnect is an idle policy or an administrator or a session limit, and a client that
reconnects after being asked to leave is a client arguing with an operator. What is still missing for
the automatic path is not the safety — that is the pinning, which is done — but backoff, an attempt
limit, and a way for it not to surprise somebody. `KeepaliveTimeout` also fires when a laptop wakes
from sleep while the server-side session is still alive, so an eager retry there orphans a session.

The lesson worth keeping from the middle of the project: a protocol crate passing its tests is not a
feature. Wiring is the phase.

## What remains, measured against the plan

Taken from the repository rather than from the phase list below, because the two drift. Everything
here was checked in the source at the time of writing.

### Protocols

The session dialog has fifteen protocol tabs, because that is what the reference has. Two of them
open a session.

| Protocol | State |
|---|---|
| SSH | done: every auth method, jump chains, `ssh_config`, `known_hosts`, keepalive, forwards, reconnect |
| Local shell | done, including WSL distribution discovery |
| RDP | built end to end; verified against a live server up to authentication only |
| Telnet · Serial · Rlogin | absent. All three are byte streams, so they plug into `Transport` and the terminal pane already renders them |
| VNC | absent. Frame-based, so it needs a second helper process — `helper-surface` already takes the helper's name as a parameter for exactly this |
| SFTP | absent, and it is the release condition for 1.0. Needs a pane kind that is neither a terminal nor a surface |
| FTP | absent; would share whatever browser SFTP gets |
| SPICE · S3 · Browser | absent, and none is scheduled. The Browser session type collides with "no webview" and is an open question |

### Phase 1, which was skipped past

The interface was built before the renderer it assumed.

* `term-render` still paints through egui's text layout. The GPU glyph atlas (`swash` → `etagere` →
  `wgpu`, with damage tracking) does not exist, and neither do the things that depend on it: ligatures,
  colour emoji, and bold or italic at all — egui's bundled monospace face has no variants.
* No scrollback search.
* **No splits.** The Split menu and ribbon button exist and do nothing.
* No layout restoration, no multi-window viewports, no terminal themes.
* Application configuration does not persist. The session tree does; nothing else.

### Chrome measured only at the top

* Eight menus have the right titles and placeholder items.
* The session dialog has all fifteen tabs; only `Basic` is measured. Advanced, Terminal, Network and
  Bookmark are unmeasured for every protocol.
* The Configuration dialog has its seven tabs; only `General` is measured.
* The Tools and Macros sidebar panels are catalogues with no behaviour behind them.

### Whole phases untouched

X server (phase 6, the most expensive), multi-exec, macros, session logging and recording, tmux
control mode, zmodem/trzsz, the command palette, WASM plugins.

### Packaging: nothing exists

CI uploads bare binaries. There is no MSI or NSIS, no deb, rpm, AppImage or Flatpak, no code signing,
no `.desktop` entry and no application icon. Until this exists there is a repository rather than a
product — and it forces a decision that is currently unmade: `helper_path` looks for `bestterm-rdp`
beside the running executable, which a `.deb` splitting `/usr/bin` from `/usr/lib` has to answer.

### Linux, which is worse than the build status suggests

CI builds and runs the whole test suite on `x86_64-unknown-linux-gnu`, with the GUI's dependencies
installed, so the code links. **The window has never been opened on Linux.** Tests do not start a GUI,
and every bug found by running this application so far — a terminal that stayed blank, a tab labelled
`sC:\Windows\...` — was found by running it.

Specifically outstanding:

* Wayland versus X11: eframe supports both, but scaling, clipboard and IME differ between them and
  none of it has been looked at.
* The vault has no OS keystore backend on either platform — `keyring` is not a dependency at all — so
  the master password is typed once per run instead of coming from DPAPI or the Secret Service.
* No guaranteed monospace font: the bundled egui face is what gets used.
* Ubuntu, Debian and Arch differ in glibc and in library versions. A binary built on `ubuntu-latest`
  is not automatically a binary that starts on Debian stable, and Arch is rolling.

### Suggested order

1. **Open the window on Linux.** One evening, and it will say more than a week of reasoning. Expect
   three or four things at the level of "wrong size" and "no font".
2. **Packaging** — deb, AppImage, MSI. It also forces the helper-location decision.
3. **OS keystore for the vault** — small, and removes a daily irritation on both systems.
4. **SFTP** — the most conspicuous missing feature, and cheaper than RDP or VNC.
5. **The terminal renderer and splits** — the part of phase 1 that was skipped.
6. **VNC** — closes phase 3 and the 0.9 beta.

Items 1–4 give something usable every day on both systems. 5–6 give the public beta. Full parity
remains what the note at the top of this file says it is.

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

Telnet, serial, rlogin, RSH, FTP, Mosh, SPICE. Also the test of whether `Transport` leaks.

## Phase 6 — X11

The SSH x11 channel, xauth and display allocation · orchestration of the running Xorg/XWayland on
Linux · a bundled VcXsrv build on Windows with lifecycle management · XDMCP last. The most expensive
phase in the plan; scoped as orchestration of an existing X server, never as writing one.

## Phase 7 — the tool set

The gap a re-reading of [`ui-parity.md`](ui-parity.md) exposed: every ribbon button there has either a
phase against it or a written-down reason for being left out — except `Servers` and `Tools`, which had
neither, and which in the original hide more functionality than any other two buttons. Recording them
here so they are scheduled rather than discovered.

Grouped by what people actually reach for, not by which menu they sit under:

* **Needed for ordinary SSH use, and cheap.** An SSH key generator (people have to make keys
  somehow); an SSH *agent* of our own, since phase 2 only consumes whatever agent the machine already
  runs; a text editor, which is also what phase 4's remote file editing opens.
* **Network utilities.** Port and network scanner, ping, traceroute, whois, DNS lookup, listening
  ports, ARP table, Wake-on-LAN, serial port monitor.
* **Local servers.** TFTP, NFS, FTP, SFTP/SSH, Telnet, HTTP, and scheduled jobs. NFS in particular
  pairs with phase 6: it is half of how the original is used for Unix development from Windows.

The first group is small enough to pull forward into phases 2 and 4; the other two are genuinely
large. Sequencing is an open question, deliberately left open rather than guessed at.

## Phase 8 — power features

Multi-exec broadcast · macros · session logging and recording · tmux control mode · zmodem/trzsz ·
command palette and history-based completion.

## Phase 9 — extensibility → 2.0

`plugin-host` on `wasmtime` with a capability model, SDK and scaffolding · configuration sync over
Git · team vaults.

## Open questions

* **The Browser session type.** MobaXterm can open a web browser as a session. Full parity therefore
  means shipping a browser engine, which contradicts the decision to be native with no webview — the
  only place where those two commitments actually collide. Deciding it means choosing which of the two
  matters more; nothing else in the plan depends on the answer, so it can wait, but it cannot be
  resolved by implementation cleverness.
* **Portable mode.** Packaging produces a portable zip, but what people mean by portable is
  configuration living beside the executable instead of in `AppData`. `crates/config` resolves paths
  through `directories`; an override is a small change and an unmade decision.
* **Sequencing of phase 7.** See the note there.

## Permanent non-goals

* Cloning Cygwin / `MobApt`. WSL and PowerShell detection instead.
* Writing an X server, an RDP stack or a VNC stack from scratch.
* Windows 7 and niche distribution support at launch.
* Using MobaXterm's icons, artwork or name.
