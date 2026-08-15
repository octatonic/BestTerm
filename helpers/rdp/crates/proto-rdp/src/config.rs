//! Turning a session's settings into what IronRDP's connector wants.
//!
//! `connector::Config` has thirty fields and no `Default`, and several of them are decisions rather
//! than transcriptions. This module is where those decisions are made once, in one place, with the
//! reasoning next to them — rather than being spread through whatever code happens to open a
//! connection.

use bestterm_ipc_frame::ConnectRequest;
use ironrdp_connector::{Config, Credentials, DesktopSize};
use ironrdp_pdu::gcc::KeyboardType;
use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};

/// The largest desktop dimension RDP can express.
///
/// The wire fields are sixteen bits. A request beyond that is a mistake somewhere upstream, and
/// truncating it silently would hand the server a desktop size nobody asked for.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

/// The smallest desktop RDP servers reliably accept.
///
/// Below this, Windows negotiates something else and the client is left rendering a framebuffer that
/// does not match what it asked for.
pub const MIN_DIMENSION: u32 = 200;

/// What went wrong before a single byte was sent.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A desktop dimension RDP cannot carry.
    ///
    /// The bounds are [`MIN_DIMENSION`] and [`MAX_DIMENSION`].
    #[error("a {width}x{height} desktop is outside the range RDP can express")]
    DesktopSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },

    /// No user name to authenticate as.
    ///
    /// Refused here rather than at the server, which answers an empty user name with a generic
    /// failure that reads like a wrong password.
    #[error("a user name is required")]
    NoUsername,
}

/// The platform this build reports to the server.
///
/// Servers log it and some licence policies key off it. Reporting the truth costs nothing and makes a
/// session traceable to the client that opened it.
#[cfg(windows)]
const fn platform() -> MajorPlatformType {
    MajorPlatformType::WINDOWS
}

/// The platform this build reports to the server.
///
/// Every supported target that is not Windows is Unix. A future macOS build would want `MACINTOSH`
/// here; until there is one, claiming it would be a lie.
#[cfg(not(windows))]
const fn platform() -> MajorPlatformType {
    MajorPlatformType::UNIX
}

/// Build the connector configuration for `request`.
///
/// # The result must never be logged
///
/// `Config` derives `Debug`, and its `credentials` hold the password as a plain `String`. Printing one
/// puts the password in the log. [`ConnectRequest`] guards itself against exactly that; the moment its
/// contents are translated into a `Config`, the guard is gone. Nothing here can enforce it, so it is
/// written down: pass this straight to the connector and print nothing.
///
/// # Security
///
/// The two security options are derived from one setting rather than exposed separately, and the
/// derivation is the point of it. IronRDP's own documentation recommends turning *off* the legacy
/// TLS-only protocol when the server can do Network Level Authentication, because with it available
/// the whole connection sequence completes before anyone has authenticated — every static channel
/// joined, clipboard and drive redirection included. So:
///
/// * NLA on — `enable_credssp`, and `enable_tls` off, so the client cannot be talked down to the
///   weaker protocol by a server that offers both.
/// * NLA off — `enable_tls`, because that is the only thing left, and the credentials will be typed
///   into the remote login screen instead of proven up front.
///
/// Offering both at once would mean the *server* chooses how much security to use, which is the wrong
/// way round.
pub fn build(request: &ConnectRequest) -> Result<Config, ConfigError> {
    if request.username.is_empty() {
        return Err(ConfigError::NoUsername);
    }
    let desktop_size = desktop_size(request)?;

    Ok(Config {
        desktop_size,
        // Zero means "do not send one", which lets the server apply its own scaling. Honouring a
        // display's real scale factor needs the UI layer to say what it is; until it does, inventing
        // a number here would make text the wrong size on every high-density screen.
        desktop_scale_factor: 0,
        enable_tls: !request.enable_credssp,
        enable_credssp: request.enable_credssp,
        credentials: Credentials::UsernamePassword {
            username: request.username.clone(),
            // The one place the password leaves `Secret`. It goes no further than the connector,
            // which needs the bytes to prove them.
            password: request.password.expose().to_owned(),
        },
        domain: request.domain.clone(),
        client_build: 0,
        client_name: request.client_name.clone(),
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: request.keyboard_layout,
        ime_file_name: String::new(),
        // No bitmap codec configuration: the server then falls back to the encodings every RDP
        // server has, which is the right starting point. RemoteFX and the rest are worth turning on
        // once there is a picture to compare them against.
        bitmap: None,
        dig_product_id: String::new(),
        // Windows clients report the path of their own ActiveX control here and some servers log it.
        // Reporting the real one keeps the field meaningful.
        client_dir: r"C:\Windows\System32\mstscax.dll".to_owned(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        platform: platform(),
        hardware_id: None,
        // Left to the connector, which derives a cookie from the user name — the same thing a
        // Windows client sends, and what load balancers in front of a farm route on.
        request_data: None,
        // Autologon is set by the connector's own logic when credentials are present; forcing it
        // here would also set it for the cases where it must not be.
        autologon: false,
        enable_audio_playback: false,
        // Bandwidth spent on things nobody misses over a network link. Font smoothing is deliberately
        // left on, from IronRDP's default: turning it off makes remote text visibly worse, which is
        // the one thing a person looks at all day.
        performance_flags: PerformanceFlags::default(),
        // No licence cache yet. A per-device licence is re-issued on every connection without one,
        // which works but leaves a trail in the server's licensing log.
        license_cache: None,
        // An empty timezone means the server keeps its own. Sending a real one requires reading the
        // local zone and converting it to the Win32 shape, which is worth doing properly later
        // rather than approximately now.
        timezone_info: TimezoneInfo::default(),
        // MPPC with a 64 KiB history: supported by every server since RDP 5 and a clear win on a
        // link slow enough to matter. The newer codecs are negotiated separately, per bitmap.
        compression_type: Some(CompressionType::K64),
        // The server draws its own pointer into the framebuffer. Drawing it locally is smoother but
        // needs the cursor shape plumbed to the windowing layer, so it waits until there is a window.
        enable_server_pointer: false,
        pointer_software_rendering: true,
        // No multitransport: the UDP transports need their own sockets and give up reliability for
        // latency, which is a trade to make on purpose and not by default.
        multitransport_flags: None,
    })
}

/// Narrow a requested desktop size to what RDP can carry, refusing what it cannot.
fn desktop_size(request: &ConnectRequest) -> Result<DesktopSize, ConfigError> {
    let width = request.desktop_size.width;
    let height = request.desktop_size.height;
    let out_of_range = |value: u32| !(MIN_DIMENSION..=MAX_DIMENSION).contains(&value);

    if out_of_range(width) || out_of_range(height) {
        return Err(ConfigError::DesktopSize { width, height });
    }

    // Checked above, so both conversions hold; `try_from` rather than `as` so a change to the range
    // cannot turn into a silent truncation.
    match (u16::try_from(width), u16::try_from(height)) {
        (Ok(width), Ok(height)) => Ok(DesktopSize { width, height }),
        _ => Err(ConfigError::DesktopSize { width, height }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bestterm_core_vault::Secret;
    use bestterm_surface::FrameSize;

    fn request() -> ConnectRequest {
        ConnectRequest {
            host: "rdp.int".to_string(),
            port: 3389,
            username: "administrator".to_string(),
            domain: Some("CORP".to_string()),
            password: Secret::new("hunter2".to_string()),
            desktop_size: FrameSize::new(1920, 1080),
            enable_credssp: true,
            keyboard_layout: 0x0409,
            client_name: "bestterm".to_string(),
            known_server_key: None,
        }
    }

    #[test]
    fn the_requested_desktop_and_identity_reach_the_connector() {
        let config = build(&request()).expect("builds");

        assert_eq!(config.desktop_size.width, 1920);
        assert_eq!(config.desktop_size.height, 1080);
        assert_eq!(config.domain.as_deref(), Some("CORP"));
        assert_eq!(config.keyboard_layout, 0x0409);
        assert_eq!(config.client_name, "bestterm");

        match &config.credentials {
            Credentials::UsernamePassword { username, password } => {
                assert_eq!(username, "administrator");
                assert_eq!(password, "hunter2");
            }
            other => panic!("expected a username and password, got {other:?}"),
        }
    }

    #[test]
    fn network_level_authentication_switches_off_the_legacy_protocol() {
        // The security decision this module exists to make, and the one worth a test: offering both
        // would let the server pick the weaker one.
        let config = build(&request()).expect("builds");
        assert!(config.enable_credssp, "NLA was asked for");
        assert!(
            !config.enable_tls,
            "the legacy TLS-only protocol must not also be offered"
        );
    }

    #[test]
    fn turning_off_network_level_authentication_leaves_the_legacy_protocol() {
        // Old servers cannot do NLA at all. Refusing both would mean refusing to connect.
        let mut without = request();
        without.enable_credssp = false;

        let config = build(&without).expect("builds");
        assert!(!config.enable_credssp);
        assert!(config.enable_tls, "something has to be offered");
    }

    #[test]
    fn a_desktop_too_large_for_the_wire_is_refused_rather_than_truncated() {
        // The wire fields are sixteen bits. Truncating 70000 to 4464 would hand the server a size
        // nobody asked for and leave the picture inexplicably wrong.
        let mut huge = request();
        huge.desktop_size = FrameSize::new(70_000, 1080);

        let error = build(&huge).expect_err("too wide");
        assert_eq!(
            error,
            ConfigError::DesktopSize {
                width: 70_000,
                height: 1080
            }
        );
    }

    #[test]
    fn a_desktop_too_small_to_negotiate_is_refused() {
        let mut tiny = request();
        tiny.desktop_size = FrameSize::new(16, 16);
        assert!(build(&tiny).is_err());
    }

    #[test]
    fn the_extremes_of_the_allowed_range_are_allowed() {
        // Off-by-one at a boundary is the classic way a range check refuses something valid.
        for size in [
            FrameSize::new(MIN_DIMENSION, MIN_DIMENSION),
            FrameSize::new(MAX_DIMENSION, MAX_DIMENSION),
        ] {
            let mut request = request();
            request.desktop_size = size;
            let config = build(&request).unwrap_or_else(|error| panic!("{size:?}: {error}"));
            assert_eq!(u32::from(config.desktop_size.width), size.width);
            assert_eq!(u32::from(config.desktop_size.height), size.height);
        }
    }

    #[test]
    fn an_empty_user_name_is_refused_here_rather_than_by_the_server() {
        // A server answers an empty user name with a generic failure that reads like a wrong
        // password, and sends whoever sees it looking in the wrong place.
        let mut nameless = request();
        nameless.username = String::new();
        // Compared as the error rather than the whole `Result`: `Config` has no `PartialEq`, and it
        // should not gain one — see the warning on `build`.
        assert_eq!(build(&nameless).unwrap_err(), ConfigError::NoUsername);
    }

    #[test]
    fn an_absent_domain_stays_absent() {
        // A local account and an account in a domain named "" are different things to a server.
        let mut local = request();
        local.domain = None;
        assert_eq!(build(&local).expect("builds").domain, None);
    }

    #[test]
    fn the_platform_reported_is_the_one_this_build_runs_on() {
        let config = build(&request()).expect("builds");
        #[cfg(windows)]
        assert_eq!(config.platform, MajorPlatformType::WINDOWS);
        #[cfg(not(windows))]
        assert_eq!(config.platform, MajorPlatformType::UNIX);
    }
}
