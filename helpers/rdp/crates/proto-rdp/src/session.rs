//! Opening an RDP session: the handshake, up to the point where frames start arriving.
//!
//! # The shape of an RDP connection
//!
//! It happens in two halves with a change of transport in the middle, which is why this reads less
//! like "connect and authenticate" than SSH does:
//!
//! 1. Plain TCP. The client and server negotiate which security protocol to use. This is
//!    [`ironrdp_tokio::connect_begin`], and it stops as soon as the answer is "upgrade to TLS".
//! 2. TLS is established over the same socket.
//! 3. Everything else — CredSSP, licensing, capability exchange, channel joining — runs inside TLS.
//!    This is [`ironrdp_tokio::connect_finalize`].
//!
//! The server's public key is extracted between the two halves and used twice: once by
//! [`crate::server_key`], to decide whether this is the machine we expect, and once by CredSSP, which
//! binds the authentication exchange to it so that a machine-in-the-middle cannot replay it.
//!
//! # Kerberos
//!
//! CredSSP may need to reach a key distribution centre, which IronRDP delegates to a
//! [`ironrdp_tokio::NetworkClient`]. This build supplies one that refuses: password authentication
//! (NTLM) needs no such request, and supporting Kerberos properly means an HTTP client, a realm
//! configuration and somewhere to discover the KDC — a feature, not a detail. Refusing with a clear
//! message beats pulling in an HTTP stack that nothing yet uses.

use bestterm_ipc_frame::ConnectRequest;
use ironrdp_connector::{
    ClientConnector, ConnectionResult, ConnectorError, ConnectorResult, ServerName, sspi,
};
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_tokio::{NetworkClient, TokioFramed};
use tokio::net::TcpStream;

use crate::config;
use crate::server_key::{Outcome, ServerKeyChecker, Verdict, Verifier};

/// The framed stream a session runs on once TLS is up.
pub type SessionStream = TokioFramed<ironrdp_tls::TlsStream<TcpStream>>;

/// What went wrong while opening a session.
#[derive(Debug, thiserror::Error)]
pub enum RdpError {
    /// The request could not be turned into a valid configuration.
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    /// The socket or the TLS layer failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The server is not the one recorded for this address.
    ///
    /// Kept distinct from an authentication failure, exactly as on the SSH side: one means the
    /// *server* was not accepted, the other means the credential was wrong, and sending somebody to
    /// check the wrong one wastes their afternoon.
    #[error("the server's key was not accepted ({verdict})")]
    ServerKeyRejected {
        /// What the store said about the key.
        verdict: VerdictSummary,
    },

    /// The certificate held no public key that could be read.
    ///
    /// Not something a working server does. Reported rather than ignored because the public key is
    /// what both the pinning check and CredSSP rely on, and continuing without one would silently
    /// drop both.
    #[error("the server's certificate carried no usable public key")]
    NoServerPublicKey,

    /// Bytes were left in the buffer at the moment TLS was due to start.
    ///
    /// Anything still buffered would be read as plaintext after the upgrade, so this cannot be
    /// waved through. `ironrdp-async` treats it as a debug assertion, which is a panic in one build
    /// profile and silent data loss in the other; a helper process should do neither.
    #[error("{count} unexpected byte(s) arrived before the TLS upgrade")]
    LeftoverBeforeUpgrade {
        /// How many bytes were left over.
        count: usize,
    },

    /// The protocol exchange failed.
    #[error("rdp: {0}")]
    Protocol(String),

    /// The server would not accept the credential.
    ///
    /// Told apart from [`RdpError::Protocol`] because it is the one failure whose fix is somebody
    /// retyping something, and from [`RdpError::ServerKeyRejected`] because that one means the
    /// *server* was refused. Measured against a real host: a wrong password arrived as
    /// `rdp: [CredSSP @ C:\Users\...\connector.rs:107] CredSSP`, which names neither the problem nor
    /// anything to do about it, and leaks the path the binary was built from.
    #[error(
        "the server rejected the user name or password{}",
        detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
    )]
    CredentialRejected {
        /// What the security layer said, when it said anything worth repeating.
        detail: Option<String>,
    },

    /// The server asked for a Kerberos exchange this build cannot perform.
    #[error("this build cannot authenticate with Kerberos; use a password")]
    KerberosUnsupported,
}

/// A verdict, flattened for an error message.
///
/// `Verdict::Changed` carries the expected fingerprint, which is exactly what somebody needs to see
/// in the message — and is why this exists rather than the error holding a bare string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerdictSummary {
    /// Nothing was recorded for this server.
    Unknown,
    /// A different key was recorded.
    Changed {
        /// The fingerprint that was expected, as it is displayed to a person.
        expected: String,
    },
    /// The key was revoked.
    Revoked,
}

impl std::fmt::Display for VerdictSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("this server has not been seen before"),
            Self::Changed { expected } => {
                write!(f, "expected {expected}")
            }
            Self::Revoked => f.write_str("this key was revoked"),
        }
    }
}

impl VerdictSummary {
    /// Summarise `verdict`, which must not be [`Verdict::Trusted`].
    ///
    /// A trusted verdict never reaches an error, so it collapses to `Unknown` here rather than
    /// widening the type with a variant that cannot occur.
    fn of(verdict: &Verdict) -> Self {
        match verdict {
            Verdict::Changed { expected } => Self::Changed {
                expected: expected.to_string(),
            },
            Verdict::Revoked => Self::Revoked,
            Verdict::Trusted | Verdict::Unknown => Self::Unknown,
        }
    }
}

/// An RDP session that has finished connecting.
pub struct Connected {
    /// What the server agreed to: desktop size, channels, compression.
    pub result: ConnectionResult,
    /// What was decided about the server's key, so the caller can store it.
    pub server_key: Outcome,
    /// The stream everything else runs over.
    pub stream: SessionStream,
}

impl std::fmt::Debug for Connected {
    /// Written by hand: neither the stream nor the channel set has a useful `Debug`, and the two
    /// things worth seeing in a log are the size and whether the key needs writing down.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connected")
            .field("width", &self.result.desktop_size.width)
            .field("height", &self.result.desktop_size.height)
            .field("io_channel_id", &self.result.io_channel_id)
            .field("store_server_key", &self.server_key.should_store())
            .finish_non_exhaustive()
    }
}

/// A network client that performs no requests.
///
/// See the module documentation. The trait exists so CredSSP can reach a key distribution centre;
/// refusing is honest, and the message says what to do instead.
struct NoKerberos;

impl NetworkClient for NoKerberos {
    async fn send(
        &mut self,
        request: &sspi::generator::NetworkRequest,
    ) -> ConnectorResult<Vec<u8>> {
        tracing::warn!(url = %request.url, "refused a Kerberos request: unsupported in this build");
        Err(ironrdp_connector::general_err!(
            "Kerberos is not supported by this build"
        ))
    }
}

/// Open a session to the server `request` names.
///
/// The server's key is checked against `checker` after TLS comes up and before any credential is
/// sent. That ordering matters: `ironrdp-tls` deliberately accepts whatever certificate arrives, so
/// this is the only point at which the wrong machine can be turned away, and it has to happen while
/// the password is still on this side of the wire.
pub async fn connect<V: Verifier>(
    request: &ConnectRequest,
    checker: &ServerKeyChecker<V>,
) -> Result<Connected, RdpError> {
    let config = config::build(request)?;

    let stream = TcpStream::connect((request.host.as_str(), request.port)).await?;
    let client_addr = stream.local_addr()?;
    tracing::debug!(host = %request.host, port = request.port, "rdp: connected");

    let mut framed = TokioFramed::new(stream);
    // The display control channel is attached here or never. It rides on the dynamic virtual
    // channel multiplexer, the connector registers neither by itself, and without both of them
    // `ActiveStage::encode_resize` returns `None` for the whole life of the session -- silently, as
    // a server that simply does not support resizing. Attaching it costs one channel; not attaching
    // it costs the feature.
    //
    // The callback answers the server's capability announcement. Nothing is requested back: the
    // monitor layout this client sends is the one `encode_resize` builds, and sending a second one
    // from here would only race it.
    let mut connector = ClientConnector::new(config, client_addr).with_static_channel(
        DrdynvcClient::new()
            .with_dynamic_channel(DisplayControlClient::new(|_capabilities| Ok(Vec::new()))),
    );

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .map_err(protocol)?;

    // Taken apart by hand rather than with `into_inner_no_leftover`, which asserts in debug builds
    // and discards in release ones.
    let (plain, leftover) = framed.into_inner();
    if !leftover.is_empty() {
        return Err(RdpError::LeftoverBeforeUpgrade {
            count: leftover.len(),
        });
    }

    let (tls, certificate) = ironrdp_tls::upgrade(plain, &request.host).await?;
    if let Some(negotiated) = ironrdp_tls::negotiated(&tls).version {
        tracing::debug!(version = %negotiated, "rdp: tls established");
    }

    let public_key = ironrdp_tls::extract_tls_server_public_key(&certificate)
        .ok_or(RdpError::NoServerPublicKey)?
        .to_vec();

    // Nothing above this line proved *which* server answered. This does.
    let server_key = checker.check(&request.host, request.port, &public_key);
    if !server_key.allows_connection() {
        return Err(RdpError::ServerKeyRejected {
            verdict: VerdictSummary::of(&server_key.verdict),
        });
    }

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut stream = TokioFramed::new(tls);

    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut stream,
        &mut NoKerberos,
        ServerName::new(&request.host),
        public_key,
        None,
    )
    .await
    .map_err(protocol)?;

    tracing::info!(
        host = %request.host,
        width = result.desktop_size.width,
        height = result.desktop_size.height,
        "rdp: session established"
    );

    Ok(Connected {
        result,
        server_key,
        stream,
    })
}

/// Turn a connector error into something a person can act on.
///
/// Not `error.to_string()`, which was the first version of this. `ironrdp-error`'s `Display` writes
/// `[{context} @ {file}:{line}] {kind}`, so the message carried the absolute path of the machine the
/// binary was built on and ended with the word `CredSSP` -- the name of a protocol rather than a
/// description of anything. Only the kind is read here, and the one kind with a specific answer gets a
/// specific error.
fn protocol(error: ConnectorError) -> RdpError {
    use ironrdp_connector::ConnectorErrorKind;
    use sspi::ErrorKind;

    match error.kind() {
        // The NT status first, because it is where the actual answer is. Measured against a real
        // server: a non-existent account produced `error_type: InternalError` with the description
        // "CredSSP server returned an error status" and `nstatus: 0xc000006d` -- so reading only the
        // error type reports a protocol failure for what is plainly a wrong password.
        ConnectorErrorKind::Credssp(inner) if inner.nstatus.is_some() => {
            let status = inner.nstatus.expect("checked in the guard");
            match logon_problem(status) {
                Some(detail) => RdpError::CredentialRejected {
                    detail: Some(detail.to_string()),
                },
                None => RdpError::Protocol(format!(
                    "the secure login exchange failed (status {:#010x})",
                    status.0
                )),
            }
        }
        // Every way the security layer says "that is not a valid login" without an NT status to go
        // with it. `MessageAltered` is in the list because sspi's own documentation says it is used
        // for invalid credentials.
        ConnectorErrorKind::Credssp(inner)
            if matches!(
                inner.error_type,
                ErrorKind::LogonDenied
                    | ErrorKind::UnknownCredentials
                    | ErrorKind::NoCredentials
                    | ErrorKind::MessageAltered
                    | ErrorKind::WrongCredentialHandle
            ) =>
        {
            RdpError::CredentialRejected {
                detail: describe(&inner.description),
            }
        }
        // Anything else from the security layer keeps its own words, which are more specific than
        // "CredSSP" and name no source file.
        ConnectorErrorKind::Credssp(inner) => {
            RdpError::Protocol(match describe(&inner.description) {
                Some(detail) => format!("the secure login exchange failed: {detail}"),
                None => format!("the secure login exchange failed ({:?})", inner.error_type),
            })
        }
        ConnectorErrorKind::AccessDenied => RdpError::CredentialRejected {
            detail: Some("the server refused this account".to_string()),
        },
        // The remaining kinds name a stage of the state machine. Their own `Display` is the useful
        // part and carries no path.
        other => RdpError::Protocol(other.to_string()),
    }
}

/// What an NT status says about a login, in words, or `None` if it is not about one.
///
/// Worth distinguishing rather than collapsing to "wrong password": a locked-out account, an expired
/// password and an account that exists but is not allowed to sign in remotely are three different
/// afternoons, and Windows already told us which. The generic `LOGON_FAILURE` deliberately does not
/// say whether the user or the password was wrong, because the server deliberately does not say.
fn logon_problem(status: sspi::credssp::NStatusCode) -> Option<&'static str> {
    use sspi::credssp::NStatusCode as S;

    Some(match status {
        S::LOGON_FAILURE => return Some("the user name or password is not correct"),
        S::WRONG_PASSWORD => "the password is not correct",
        S::NO_SUCH_USER => "there is no such account on this server",
        S::ACCOUNT_DISABLED => "the account is disabled",
        S::ACCOUNT_LOCKED_OUT => "the account is locked out",
        S::PASSWORD_EXPIRED => "the password has expired",
        S::PASSWORD_MUST_CHANGE => "the password has to be changed before signing in",
        S::PASSWORD_RESTRICTION => "the password does not meet this server's requirements",
        S::INVALID_LOGON_HOURS => "the account is not allowed to sign in at this time of day",
        S::INVALID_WORKSTATION => "the account is not allowed to sign in from this machine",
        S::LOGON_TYPE_NOT_GRANTED => "the account is not allowed to sign in over remote desktop",
        S::ACCOUNT_RESTRICTION => "the account is restricted from signing in",
        S::NO_LOGON_SERVERS => "the server could not reach a domain controller",
        _ => return None,
    })
}

/// A description worth repeating, or nothing.
///
/// Security layers routinely produce an empty string or their own protocol's name here, and passing
/// either through turns a clear message into a confusing one.
fn describe(description: &str) -> Option<String> {
    let trimmed = description.trim();
    (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("credssp")).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_key::KeyFingerprint;

    fn fingerprint() -> KeyFingerprint {
        KeyFingerprint::of(&[7u8; 64])
    }

    #[test]
    fn a_changed_key_says_which_one_was_expected() {
        // The whole point of carrying a summary instead of a string: a person deciding whether this
        // is the rebuild they did on Tuesday needs to see the fingerprint they trusted.
        let expected = fingerprint();
        let summary = VerdictSummary::of(&Verdict::Changed { expected });

        let message = RdpError::ServerKeyRejected {
            verdict: summary.clone(),
        }
        .to_string();

        assert!(message.contains(&expected.to_string()), "{message}");
        assert_eq!(
            summary,
            VerdictSummary::Changed {
                expected: expected.to_string()
            }
        );
    }

    #[test]
    fn an_unknown_server_reads_differently_from_a_revoked_one() {
        let unknown = VerdictSummary::of(&Verdict::Unknown).to_string();
        let revoked = VerdictSummary::of(&Verdict::Revoked).to_string();

        assert!(unknown.contains("not been seen"), "{unknown}");
        assert!(revoked.contains("revoked"), "{revoked}");
        assert_ne!(unknown, revoked);
    }

    #[test]
    fn a_rejected_password_says_so_and_names_no_source_file() {
        // Measured against a real server: a wrong password arrived as
        // `rdp: [CredSSP @ C:\Users\...\connector.rs:107] CredSSP`. That names neither the problem
        // nor a remedy, and it leaks the build machine's paths into the interface.
        let message = RdpError::CredentialRejected { detail: None }.to_string();
        assert!(message.contains("password"), "{message}");
        assert!(!message.contains(".rs:"), "{message}");
        assert!(!message.contains("CredSSP"), "{message}");

        let with_detail = RdpError::CredentialRejected {
            detail: Some("the account is locked out".to_string()),
        }
        .to_string();
        assert!(with_detail.contains("locked out"), "{with_detail}");
    }

    #[test]
    fn the_nt_status_is_what_says_which_login_problem_it_was() {
        use sspi::credssp::NStatusCode as S;

        // 0xc000006d is what a real server returned for an account that does not exist. It arrived
        // alongside `error_type: InternalError` and the description "CredSSP server returned an error
        // status", so reading the error type alone called a wrong password a protocol failure.
        assert_eq!(
            logon_problem(S::LOGON_FAILURE),
            Some("the user name or password is not correct")
        );

        // The distinctions worth keeping: these are three different afternoons.
        assert_eq!(
            logon_problem(S::ACCOUNT_LOCKED_OUT),
            Some("the account is locked out")
        );
        assert_eq!(
            logon_problem(S::PASSWORD_EXPIRED),
            Some("the password has expired")
        );
        assert_eq!(
            logon_problem(S::LOGON_TYPE_NOT_GRANTED),
            Some("the account is not allowed to sign in over remote desktop")
        );

        // Anything that is not about a login is not turned into one.
        assert_eq!(logon_problem(S::SUCCESS), None);
        assert_eq!(logon_problem(sspi::credssp::NStatusCode(0xc000_0001)), None);
    }

    #[test]
    fn no_two_login_problems_read_the_same() {
        use sspi::credssp::NStatusCode as S;

        // A message shared between two causes is a message that sends somebody to check the wrong
        // thing.
        let statuses = [
            S::LOGON_FAILURE,
            S::WRONG_PASSWORD,
            S::NO_SUCH_USER,
            S::ACCOUNT_DISABLED,
            S::ACCOUNT_LOCKED_OUT,
            S::PASSWORD_EXPIRED,
            S::PASSWORD_MUST_CHANGE,
            S::PASSWORD_RESTRICTION,
            S::INVALID_LOGON_HOURS,
            S::INVALID_WORKSTATION,
            S::LOGON_TYPE_NOT_GRANTED,
            S::ACCOUNT_RESTRICTION,
            S::NO_LOGON_SERVERS,
        ];
        let mut seen = std::collections::HashSet::new();
        for status in statuses {
            let detail = logon_problem(status).expect("every one of these is a login problem");
            assert!(seen.insert(detail), "{detail:?} is used for two statuses");
        }
    }

    #[test]
    fn an_empty_or_useless_description_is_not_repeated() {
        assert_eq!(describe("  "), None);
        // The security layer returns its own protocol's name here, which says nothing.
        assert_eq!(describe("CredSSP"), None);
        assert_eq!(describe("credssp"), None);
        assert_eq!(
            describe(" the account is disabled "),
            Some("the account is disabled".to_string())
        );
    }

    #[test]
    fn a_rejected_credential_reads_differently_from_a_rejected_server() {
        // The two failures somebody would otherwise go and check the wrong one of.
        let credential = RdpError::CredentialRejected { detail: None }.to_string();
        let server = RdpError::ServerKeyRejected {
            verdict: VerdictSummary::Revoked,
        }
        .to_string();
        assert_ne!(credential, server);
        assert!(server.contains("key"), "{server}");
        assert!(!credential.contains("key"), "{credential}");
    }

    #[test]
    fn a_key_problem_reads_differently_from_a_protocol_failure() {
        // Told apart on purpose: one means the server is not who it claimed, the other means the
        // exchange broke. Sending somebody to check the wrong one wastes their afternoon.
        let key = RdpError::ServerKeyRejected {
            verdict: VerdictSummary::Revoked,
        }
        .to_string();
        let protocol = RdpError::Protocol("capability exchange failed".to_string()).to_string();

        assert_ne!(key, protocol);
        assert!(key.contains("key"), "{key}");
    }

    #[test]
    fn leftover_bytes_before_the_upgrade_are_counted_in_the_message() {
        // Whoever reads this needs to know it was a protocol surprise and not a network error.
        let error = RdpError::LeftoverBeforeUpgrade { count: 12 };
        let message = error.to_string();
        assert!(message.contains("12"), "{message}");
        assert!(message.contains("TLS"), "{message}");
    }

    #[test]
    fn a_configuration_error_is_reported_as_itself() {
        // `#[error(transparent)]`: a desktop that RDP cannot express is a configuration problem, and
        // wrapping it in "rdp: ..." would suggest the server said something about it.
        let inner = config::ConfigError::NoUsername;
        let expected = inner.to_string();
        assert_eq!(RdpError::Config(inner).to_string(), expected);
    }
}
