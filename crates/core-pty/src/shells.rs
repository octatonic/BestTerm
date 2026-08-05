//! Discovery of the shells available on this machine.
//!
//! BestTerm deliberately does **not** bundle a Unix environment the way MobaXterm bundles Cygwin.
//! On Windows the modern answer is WSL, which the user already has, plus PowerShell and cmd. That
//! decision is recorded in `docs/ROADMAP.md` under permanent non-goals; this module is its
//! implementation.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Family a shell belongs to. Drives the icon and a few behavioural defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShellKind {
    /// Windows `cmd.exe`.
    Cmd,
    /// Windows PowerShell 5.x (`powershell.exe`).
    PowerShell,
    /// PowerShell 7+ (`pwsh`).
    PowerShellCore,
    /// A WSL distribution.
    Wsl,
    /// `bash`, including Git Bash on Windows.
    Bash,
    /// `zsh`.
    Zsh,
    /// `fish`.
    Fish,
    /// POSIX `sh`.
    Sh,
    /// Anything else the user configured by hand.
    Other,
}

impl ShellKind {
    /// Short stable identifier, safe to persist in configuration.
    pub fn id(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
            Self::PowerShellCore => "pwsh",
            Self::Wsl => "wsl",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Sh => "sh",
            Self::Other => "other",
        }
    }

    /// Guess the family from an executable name or full path.
    ///
    /// Public because the Windows discovery path constructs kinds explicitly while the Unix path
    /// infers them, and a user-configured custom shell needs the same inference.
    pub fn from_program(program: &str) -> Self {
        let stem = Path::new(program)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(program)
            .to_ascii_lowercase();
        match stem.as_str() {
            "cmd" => Self::Cmd,
            "powershell" => Self::PowerShell,
            "pwsh" => Self::PowerShellCore,
            "wsl" => Self::Wsl,
            "bash" => Self::Bash,
            "zsh" => Self::Zsh,
            "fish" => Self::Fish,
            "sh" | "dash" => Self::Sh,
            _ => Self::Other,
        }
    }
}

/// One launchable local shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellProfile {
    /// Stable identifier, unique within a discovery run. Used as a configuration key.
    pub id: String,
    /// What to show the user, e.g. `PowerShell 7` or `Ubuntu-22.04 (WSL)`.
    pub label: String,
    /// Executable to run.
    pub program: String,
    /// Arguments to pass.
    pub args: Vec<String>,
    /// Family, for icon selection.
    pub kind: ShellKind,
}

impl ShellProfile {
    fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        program: impl Into<String>,
        args: Vec<String>,
        kind: ShellKind,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            program: program.into(),
            args,
            kind,
        }
    }
}

/// Every shell we could find, with the one to open by default first.
///
/// Never empty: if discovery finds nothing at all it still returns a last-resort entry, because a
/// terminal application with no way to open a terminal is worse than one that fails at spawn time
/// with a clear error.
pub fn discover() -> Vec<ShellProfile> {
    let mut found = platform_discover();
    if found.is_empty() {
        found.push(fallback());
    }
    found
}

fn fallback() -> ShellProfile {
    if cfg!(windows) {
        ShellProfile::new("cmd", "Command Prompt", "cmd.exe", vec![], ShellKind::Cmd)
    } else {
        ShellProfile::new("sh", "sh", "/bin/sh", vec![], ShellKind::Sh)
    }
}

/// Search `PATH` for `program`, returning the full path if it is executable.
///
/// On Windows, `PATHEXT` is consulted so that `pwsh` finds `pwsh.exe`.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };

    for dir in std::env::split_paths(&path) {
        let direct = dir.join(program);
        if is_file(&direct) {
            return Some(direct);
        }
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_file(p: &Path) -> bool {
    p.metadata().map(|m| m.is_file()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn platform_discover() -> Vec<ShellProfile> {
    let mut out = Vec::new();

    // PowerShell 7+ first when present: it is what a user who installed it expects to get.
    if let Some(pwsh) = which("pwsh") {
        out.push(ShellProfile::new(
            "pwsh",
            "PowerShell 7",
            pwsh.to_string_lossy().into_owned(),
            vec!["-NoLogo".to_string()],
            ShellKind::PowerShellCore,
        ));
    }

    // Windows PowerShell 5.x lives at a fixed path; resolving it through PATH picks up shims.
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let ps5 = PathBuf::from(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if is_file(&ps5) {
        out.push(ShellProfile::new(
            "powershell",
            "Windows PowerShell",
            ps5.to_string_lossy().into_owned(),
            vec!["-NoLogo".to_string()],
            ShellKind::PowerShell,
        ));
    }

    let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    out.push(ShellProfile::new(
        "cmd",
        "Command Prompt",
        comspec,
        vec![],
        ShellKind::Cmd,
    ));

    for distro in wsl_distributions() {
        out.push(ShellProfile::new(
            format!("wsl:{distro}"),
            format!("{distro} (WSL)"),
            "wsl.exe",
            vec!["--distribution".to_string(), distro.clone()],
            ShellKind::Wsl,
        ));
    }

    // Git for Windows ships a usable bash; many users rely on it.
    for base in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(dir) = std::env::var(base) {
            let git_bash = PathBuf::from(dir).join("Git").join("bin").join("bash.exe");
            if is_file(&git_bash) {
                out.push(ShellProfile::new(
                    "git-bash",
                    "Git Bash",
                    git_bash.to_string_lossy().into_owned(),
                    vec!["--login".to_string(), "-i".to_string()],
                    ShellKind::Bash,
                ));
                break;
            }
        }
    }

    out
}

/// Installed WSL distribution names.
///
/// Returns an empty list when WSL is absent, which is the common case on a fresh Windows install and
/// is not an error.
#[cfg(windows)]
fn wsl_distributions() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW` — without it, probing for WSL flashes a console window on startup.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = std::process::Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(out) if out.status.success() => parse_wsl_list(&out.stdout),
        Ok(out) => {
            tracing::debug!(
                status = ?out.status,
                "wsl.exe --list returned a failure; assuming WSL is not installed"
            );
            Vec::new()
        }
        Err(err) => {
            tracing::debug!(%err, "wsl.exe not present");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn platform_discover() -> Vec<ShellProfile> {
    let mut out: Vec<ShellProfile> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let mut push = |program: String, kind: ShellKind, label: Option<String>| {
        if seen.contains(&program) {
            return;
        }
        seen.push(program.clone());
        let label = label.unwrap_or_else(|| {
            Path::new(&program)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or(&program)
                .to_string()
        });
        let id = if out.is_empty() {
            "default".to_string()
        } else {
            format!("{}:{}", kind.id(), out.len())
        };
        out.push(ShellProfile::new(id, label, program, vec![], kind));
    };

    // $SHELL is the user's stated preference and therefore the default.
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && is_file(Path::new(&shell)) {
            let kind = ShellKind::from_program(&shell);
            let name = Path::new(&shell)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("shell")
                .to_string();
            push(shell, kind, Some(format!("{name} (default)")));
        }
    }

    for candidate in [
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/bin/sh",
    ] {
        if is_file(Path::new(candidate)) {
            push(
                candidate.to_string(),
                ShellKind::from_program(candidate),
                None,
            );
        }
    }

    out
}

/// Parse the output of `wsl.exe --list --quiet`.
///
/// Split out from the process spawn so it can be tested: `wsl.exe` writes **UTF-16LE**, which is the
/// detail that breaks naive implementations — a plain `String::from_utf8_lossy` yields names with a
/// NUL between every character and every distro silently fails to launch.
pub fn parse_wsl_list(stdout: &[u8]) -> Vec<String> {
    decode_console_output(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Decode console output that may be UTF-16LE or UTF-8.
///
/// The heuristic is the one that actually works for Windows console tools: a UTF-16LE stream of
/// mostly-ASCII text has a NUL as its second byte. A BOM is stripped either way.
fn decode_console_output(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2
        && ((bytes[0] == 0xFF && bytes[1] == 0xFE) || (bytes[1] == 0 && bytes[0] != 0));

    if looks_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let decoded = String::from_utf16_lossy(&units);
        return decoded.trim_start_matches('\u{FEFF}').to_string();
    }

    String::from_utf8_lossy(bytes)
        .trim_start_matches('\u{FEFF}')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn parses_utf16le_wsl_output() {
        let raw = utf16le("Ubuntu-22.04\r\nDebian\r\ndocker-desktop\r\n");
        assert_eq!(
            parse_wsl_list(&raw),
            vec!["Ubuntu-22.04", "Debian", "docker-desktop"]
        );
    }

    #[test]
    fn parses_utf16le_with_bom() {
        let mut raw = vec![0xFF, 0xFE];
        raw.extend(utf16le("Ubuntu\r\n"));
        assert_eq!(parse_wsl_list(&raw), vec!["Ubuntu"]);
    }

    #[test]
    fn parses_utf8_output_too() {
        assert_eq!(
            parse_wsl_list(b"Ubuntu\nDebian\n"),
            vec!["Ubuntu", "Debian"]
        );
    }

    #[test]
    fn empty_output_yields_no_distros() {
        assert!(parse_wsl_list(b"").is_empty());
        assert!(parse_wsl_list(&utf16le("\r\n\r\n")).is_empty());
    }

    #[test]
    fn shell_kind_from_program_handles_windows_paths() {
        assert_eq!(
            ShellKind::from_program(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            ShellKind::PowerShellCore
        );
        assert_eq!(ShellKind::from_program("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(ShellKind::from_program("/bin/dash"), ShellKind::Sh);
        assert_eq!(ShellKind::from_program("/opt/nu"), ShellKind::Other);
    }

    #[test]
    fn discover_is_never_empty() {
        assert!(!discover().is_empty());
    }

    #[test]
    fn discovered_ids_are_unique() {
        let shells = discover();
        let mut ids: Vec<&str> = shells.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate shell ids in {shells:?}");
    }
}
