# BestTerm

A native, cross-platform remote-access workspace for Linux and Windows: tabbed terminal, session
tree, SSH, RDP, VNC, SFTP and X11 in one application. No Electron, no webview, one binary.

BestTerm reproduces the layout and interaction model that MobaXterm users already know, and can
import their existing `.mxtsessions` files — but it is an independent implementation in Rust,
licensed GPL-3.0-or-later, and ships for Linux as a first-class target rather than an afterthought.

> **Status: pre-alpha, phase 0.** The workspace skeleton, a local-shell terminal tab and the
> project's engineering guardrails exist. Nothing here has shipped yet. See
> [the roadmap](#roadmap) for what lands when.

## Why

| | MobaXterm | Tabby / electerm | XPipe | BestTerm |
|---|---|---|---|---|
| Linux as a first-class target | no | yes | yes | **yes** |
| Native, no webview | yes | no (Electron) | no (JavaFX) | **yes** |
| Own terminal engine | yes | yes | no (delegates) | **yes** |
| SSH + RDP + VNC + X11 | yes | partial | no | **planned, phases 3 and 6** |
| Open source | no | yes | yes | **yes, GPL-3.0-or-later** |

## Roadmap

Each phase is a shippable product, not a checkpoint. Detail in
[`docs/ROADMAP.md`](docs/ROADMAP.md).

| Phase | Ships |
|---|---|
| 0 | Workspace, CI on Windows + Linux, local-shell terminal tab, UI parity spec |
| 1 | GPU-rendered terminal, MobaXterm chrome, splits, themes, config |
| 2 | SSH: all auth methods, jump chains, known_hosts, tunnels, session tree, vault |
| 3 | **Public beta 0.9** — RDP (IronRDP) and VNC (libvncclient) in isolated helper processes, `.mxtsessions` import |
| 4 | **1.0** — SFTP dual-pane browser bound to the live SSH session |
| 5 | Telnet, serial, rlogin, FTP |
| 6 | X11 forwarding, bundled X server on Windows, XDMCP |
| 7 | Multi-exec, macros, session logging, tmux control mode |
| 8 | **2.0** — WASM plugins, config sync |

## Building

Requires a Rust toolchain (edition 2024, so **1.85 or newer**) and a linker.

```sh
# Linux — build dependencies for eframe/wgpu
sudo apt-get install -y build-essential pkg-config \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libxkbcommon-dev libwayland-dev libssl-dev

# Windows — MSVC toolchain and Windows SDK are required.
#   winget install Microsoft.VisualStudio.2022.BuildTools
#   (select "Desktop development with C++")
#   then: rustup default stable-x86_64-pc-windows-msvc

cargo run -p bestterm
```

`rust-toolchain.toml` selects `stable`; rustup installs it on first invocation.

## Repository layout

```
crates/
  transport/        Transport trait — byte-stream protocols (PTY, SSH, telnet, serial)
  surface/          GraphicalSurface trait — frame protocols (RDP, VNC, X11 windows)
  core-pty/         local shells: ConPTY/unix pty, WSL + pwsh + cmd discovery
  core-terminal/    TerminalEmulator trait + alacritty_terminal implementation
  term-render/      terminal grid rendering
  ui-chrome/        MobaXterm-parity chrome: theme, ribbon, sidebar, tab bar, status bar
  app-ui/           application shell: layout, tabs, wiring
apps/
  bestterm/         the binary
docs/
  ARCHITECTURE.md   layer boundaries and the reasoning behind them
  ui-parity.md      the UI specification and its acceptance checklist
```

Everything above `app-ui` and `ui-chrome` is free of GUI dependencies on purpose — see
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Contributing

`cargo fmt`, `cargo clippy --all-targets`, `cargo test` and `cargo deny check` all run in CI on
both Windows and Linux. Please make sure they pass locally first.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

BestTerm is not affiliated with, endorsed by, or derived from Mobatek's MobaXterm. "MobaXterm" is a
trademark of Mobatek. BestTerm reads the `.mxtsessions` format for interoperability and reproduces
UI *layout and interaction patterns*; it contains none of MobaXterm's code, icons, or artwork.
