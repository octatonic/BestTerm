//! Port forwarding, and the window that manages it.
//!
//! `proto-ssh` has been able to open local, remote and dynamic forwards for a while; nothing in the
//! interface could reach them. This is the reaching.
//!
//! # A tunnel belongs to a connection, not to a tab
//!
//! Which is the point of "session is not tab" in `docs/ARCHITECTURE.md`, made concrete for the first
//! time. Every SSH connection gets an id; tabs record theirs; a tunnel records the same. Closing the
//! last tab on a connection stops the tunnels on it, because a forward outliving every window that
//! could show it is a listening socket nobody knows about.
//!
//! # Why the forms are three and not one
//!
//! Local, remote and dynamic forwards look similar and mean quite different things, and the classic
//! way to get one wrong is a single form with fields that grey out. A local forward opens a socket
//! *here*; a remote forward asks the server to open one *there* and hand the connections back; a
//! dynamic forward opens a socket here and lets whatever connects choose its own destination. Only
//! the first two have a target at all, and the third's danger — that it is an open proxy into the
//! remote network — is easiest to state when it has a form of its own.

use std::sync::Arc;

use bestterm_proto_ssh::forward::{DynamicForward, LocalForward, RemoteForward};
use bestterm_proto_ssh::transport::SshConnection;

/// Identifies one SSH connection for as long as the application holds it.
///
/// A number rather than a pointer, because everything that refers to a connection — a tab, a tunnel,
/// a row in this window — has to keep referring to it without keeping it alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ConnectionId(pub(crate) u64);

/// A connection the application is holding, and what to call it.
pub(crate) struct LiveConnection {
    /// Its identity, recorded by everything that uses it.
    pub(crate) id: ConnectionId,
    /// `user@host`, which is what a person picks from a list by.
    pub(crate) label: String,
    /// The connection itself.
    pub(crate) connection: Arc<SshConnection>,
}

/// Which direction a forward runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TunnelKind {
    /// A socket here; connections go out through the server.
    Local,
    /// A socket on the server; connections come back here.
    Remote,
    /// A SOCKS proxy here; the destination is chosen per connection.
    Dynamic,
}

impl TunnelKind {
    /// Every kind, in the order the window shows them.
    pub(crate) const ALL: [Self; 3] = [Self::Local, Self::Remote, Self::Dynamic];

    /// What it is called on its tab.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Local => "Local port forwarding",
            Self::Remote => "Remote port forwarding",
            Self::Dynamic => "Dynamic port forwarding (SOCKS)",
        }
    }

    /// One sentence saying which way the traffic goes.
    ///
    /// Shown above the form rather than in a manual, because the difference between the first two is
    /// exactly what people get backwards.
    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Local => {
                "A socket opens on this machine. Whatever connects to it comes out of the SSH \
                 server and goes to the destination below."
            }
            Self::Remote => {
                "A socket opens on the SSH server. Whatever connects to it there is carried back \
                 here and sent to the destination below."
            }
            Self::Dynamic => {
                "A SOCKS5 proxy opens on this machine. Programs pointed at it choose their own \
                 destination, and the SSH server reaches it for them."
            }
        }
    }

    /// Whether this kind has a fixed destination.
    pub(crate) fn has_target(self) -> bool {
        !matches!(self, Self::Dynamic)
    }
}

/// What somebody typed into the form.
#[derive(Clone, Debug)]
pub(crate) struct TunnelForm {
    /// Which of the three.
    pub(crate) kind: TunnelKind,
    /// Where the listening socket goes.
    ///
    /// Empty means loopback, which is the safe default and the one to keep: a forward bound to every
    /// interface is reachable by everything on the network, and a database tunnel bound that way is
    /// a database exposed to the office.
    pub(crate) listen_host: String,
    /// The port to listen on. `0` asks for one to be allocated.
    pub(crate) listen_port: String,
    /// Where connections end up. Unused for a dynamic forward.
    pub(crate) target_host: String,
    /// And on which port.
    pub(crate) target_port: String,
    /// Which connection to run it over, or `None` when nothing has been picked.
    pub(crate) over: Option<ConnectionId>,
}

impl Default for TunnelForm {
    fn default() -> Self {
        Self {
            kind: TunnelKind::Local,
            listen_host: String::new(),
            listen_port: String::new(),
            target_host: String::new(),
            target_port: String::new(),
            over: None,
        }
    }
}

/// What a checked form asks for, with the destination attached to the kinds that have one.
///
/// Not a kind beside an `Option<target>`, though that is the shape the form has. Two of the three
/// kinds always have a destination and the third never does, and separating them into a field the
/// caller has to check leaves a branch for "a local forward with nowhere to go" that cannot happen
/// and still has to be written. Here it cannot be spelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TunnelPlan {
    /// Listen here, connect from the server to `target`.
    Local {
        /// Where the server should connect, unresolved.
        target: (String, u16),
    },
    /// Ask the server to listen, connect from here to `target`.
    Remote {
        /// Where this machine should connect, unresolved.
        target: (String, u16),
    },
    /// Listen here; each connection names its own destination.
    Dynamic,
}

impl TunnelPlan {
    /// Which of the three this is.
    pub(crate) fn kind(&self) -> TunnelKind {
        match self {
            Self::Local { .. } => TunnelKind::Local,
            Self::Remote { .. } => TunnelKind::Remote,
            Self::Dynamic => TunnelKind::Dynamic,
        }
    }

    /// Where connections end up, when that is fixed in advance.
    pub(crate) fn target(&self) -> Option<&(String, u16)> {
        match self {
            Self::Local { target } | Self::Remote { target } => Some(target),
            Self::Dynamic => None,
        }
    }
}

/// A form that has been checked and is ready to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TunnelRequest {
    /// What to open, and where it goes.
    pub(crate) plan: TunnelPlan,
    /// The interface to bind, already defaulted.
    pub(crate) listen_host: String,
    /// The port to bind. Zero means "allocate one".
    pub(crate) listen_port: u16,
    /// Which connection it runs over.
    pub(crate) over: ConnectionId,
}

/// Why a form could not be turned into a request.
///
/// Separate variants rather than one string so the window can point at the field that is wrong,
/// and so the messages are written once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormError {
    /// No SSH connection was chosen, or the one chosen has gone.
    NoConnection,
    /// The listening port is missing or not a port.
    ListenPort,
    /// A remote forward was asked for on port 0.
    RemotePortZero,
    /// The destination host is empty.
    TargetHost,
    /// The destination port is missing or not a port.
    TargetPort,
}

impl FormError {
    /// What to show next to the form.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NoConnection => "Choose an SSH session to run this over.",
            Self::ListenPort => "The listening port has to be a number between 0 and 65535.",
            Self::RemotePortZero => {
                "A remote forward needs a real port: the server allocates one for 0, and there is \
                 nowhere to show you which it chose yet."
            }
            Self::TargetHost => "Say where connections should end up.",
            Self::TargetPort => "The destination port has to be a number between 1 and 65535.",
        }
    }
}

impl TunnelForm {
    /// Check the form and turn it into something that can be acted on.
    ///
    /// Names are not resolved here, and deliberately: the destination of a forward is resolved by
    /// the *server*, which is usually the whole reason for the forward — `localhost` means the
    /// server's loopback, not this machine's. Resolving here would quietly change what the tunnel
    /// does.
    pub(crate) fn check(&self) -> Result<TunnelRequest, FormError> {
        let over = self.over.ok_or(FormError::NoConnection)?;

        let listen_port: u16 = self
            .listen_port
            .trim()
            .parse()
            .map_err(|_| FormError::ListenPort)?;
        if self.kind == TunnelKind::Remote && listen_port == 0 {
            return Err(FormError::RemotePortZero);
        }

        let listen_host = match self.listen_host.trim() {
            // Loopback, not every interface. See `TunnelForm::listen_host`.
            "" => default_bind(self.kind).to_string(),
            given => given.to_string(),
        };

        let plan = match self.kind {
            TunnelKind::Dynamic => TunnelPlan::Dynamic,
            directed => {
                let host = self.target_host.trim();
                if host.is_empty() {
                    return Err(FormError::TargetHost);
                }
                let port: u16 = self
                    .target_port
                    .trim()
                    .parse()
                    .map_err(|_| FormError::TargetPort)?;
                if port == 0 {
                    return Err(FormError::TargetPort);
                }
                let target = (host.to_string(), port);
                match directed {
                    TunnelKind::Local => TunnelPlan::Local { target },
                    _ => TunnelPlan::Remote { target },
                }
            }
        };

        Ok(TunnelRequest {
            plan,
            listen_host,
            listen_port,
            over,
        })
    }
}

/// Where a forward of this kind binds when nobody said.
///
/// Loopback for anything that listens on this machine. For a remote forward the empty string is what
/// the protocol means by "the server decides", which for OpenSSH means loopback unless `GatewayPorts`
/// says otherwise — so the server's own policy is respected rather than overridden from here.
fn default_bind(kind: TunnelKind) -> &'static str {
    match kind {
        TunnelKind::Local | TunnelKind::Dynamic => "127.0.0.1",
        TunnelKind::Remote => "",
    }
}

/// A forward that is running.
pub(crate) struct Tunnel {
    /// What it was asked to be.
    pub(crate) request: TunnelRequest,
    /// The session it runs over, for the list.
    pub(crate) over_label: String,
    /// Where it actually ended up listening, which is not always what was asked for.
    pub(crate) listening: String,
    /// The live forward, dropped when the tunnel is stopped.
    handle: Handle,
}

/// The three kinds of live forward, so one list can hold them.
enum Handle {
    Local(LocalForward),
    Remote(Box<RemoteForward>),
    Dynamic(DynamicForward),
}

impl Tunnel {
    /// A short description for the list.
    pub(crate) fn describe(&self) -> String {
        match self.request.plan.target() {
            Some((host, port)) => format!("{} → {host}:{port}", self.listening),
            None => format!("{} → anywhere (SOCKS5)", self.listening),
        }
    }
}

/// Everything the tunnel window knows.
#[derive(Default)]
pub(crate) struct TunnelState {
    /// Whether the window is on screen.
    pub(crate) open: bool,
    /// What is being typed.
    pub(crate) form: TunnelForm,
    /// What is running.
    pub(crate) running: Vec<Tunnel>,
    /// The last complaint about the form, cleared when it is edited.
    pub(crate) error: Option<FormError>,
    /// Something that went wrong opening or closing a tunnel.
    pub(crate) notice: Option<String>,
}

impl TunnelState {
    /// Open a forward described by `request` over `connection`.
    ///
    /// Runs on `runtime` and blocks the interface for as long as the socket takes to bind, which is
    /// the same amount of time an unbindable port takes to say so — a fraction of a millisecond
    /// either way. The forward's own work happens on tasks afterwards.
    pub(crate) fn start(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        request: TunnelRequest,
        over: &LiveConnection,
    ) {
        let connection = Arc::clone(&over.connection);

        let started = runtime.block_on(async {
            match &request.plan {
                TunnelPlan::Local {
                    target: (host, port),
                } => connection
                    .open_local_forward(
                        &request.listen_host,
                        request.listen_port,
                        host.clone(),
                        *port,
                    )
                    .await
                    .map(|forward| {
                        let listening = forward.local_addr().to_string();
                        (Handle::Local(forward), listening)
                    }),
                TunnelPlan::Remote {
                    target: (host, port),
                } => connection
                    .open_remote_forward(&request.listen_host, request.listen_port, host, *port)
                    .await
                    .map(|forward| {
                        // The server's own words for where it bound, which need not be what was
                        // asked for: an empty bind address means it chose, and OpenSSH chooses
                        // loopback unless `GatewayPorts` says otherwise.
                        let listening = format!(
                            "{}:{} on the server",
                            match forward.bind_address() {
                                "" => "*",
                                address => address,
                            },
                            forward.remote_port()
                        );
                        (Handle::Remote(Box::new(forward)), listening)
                    }),
                TunnelPlan::Dynamic => connection
                    .open_dynamic_forward(&request.listen_host, request.listen_port)
                    .await
                    .map(|forward| {
                        let listening = forward.local_addr().to_string();
                        (Handle::Dynamic(forward), listening)
                    }),
            }
        });

        match started {
            Ok((handle, listening)) => {
                tracing::info!(kind = ?request.plan.kind(), %listening, "tunnel opened");
                self.running.push(Tunnel {
                    request,
                    over_label: over.label.clone(),
                    listening,
                    handle,
                });
                self.notice = None;
            }
            Err(error) => {
                tracing::warn!(%error, "could not open the tunnel");
                self.notice = Some(error.to_string());
            }
        }
    }

    /// Stop the tunnel at `index`.
    pub(crate) fn stop(&mut self, runtime: &tokio::runtime::Runtime, index: usize) {
        if index >= self.running.len() {
            return;
        }
        let tunnel = self.running.remove(index);
        close(runtime, tunnel);
    }

    /// Stop every tunnel running over `connection`.
    ///
    /// Called when the last tab on a connection closes. A forward that outlived every window that
    /// could show it would be a listening socket nobody knows about, still carrying traffic into a
    /// network somebody thinks they have disconnected from.
    pub(crate) fn stop_all_over(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        connection: ConnectionId,
    ) {
        let mut kept = Vec::with_capacity(self.running.len());
        for tunnel in self.running.drain(..) {
            if tunnel.request.over == connection {
                tracing::info!(listening = %tunnel.listening, "closing a tunnel with its session");
                close(runtime, tunnel);
            } else {
                kept.push(tunnel);
            }
        }
        self.running = kept;
    }
}

/// Shut one tunnel down.
fn close(runtime: &tokio::runtime::Runtime, tunnel: Tunnel) {
    match tunnel.handle {
        Handle::Local(forward) => forward.stop(),
        Handle::Dynamic(forward) => forward.stop(),
        // The only one that has to say anything to the server: a remote forward exists because the
        // server was asked to listen, and it keeps listening until it is asked to stop.
        Handle::Remote(forward) => {
            if let Err(error) = runtime.block_on(forward.close()) {
                tracing::warn!(%error, "the server would not cancel a remote forward");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(kind: TunnelKind) -> TunnelForm {
        TunnelForm {
            kind,
            over: Some(ConnectionId(1)),
            listen_port: "8080".to_string(),
            target_host: "db.internal".to_string(),
            target_port: "5432".to_string(),
            ..TunnelForm::default()
        }
    }

    #[test]
    fn an_empty_bind_address_means_loopback_and_not_every_interface() {
        // The difference between a database reachable from this machine and one reachable from the
        // office. Defaulting the other way would be a one-character change nobody would notice.
        let checked = form(TunnelKind::Local).check().expect("valid");
        assert_eq!(checked.listen_host, "127.0.0.1");

        let checked = form(TunnelKind::Dynamic).check().expect("valid");
        assert_eq!(checked.listen_host, "127.0.0.1");
    }

    #[test]
    fn a_remote_forward_leaves_the_bind_address_to_the_server() {
        // Empty is what the protocol means by "you decide", and OpenSSH decides loopback unless
        // GatewayPorts says otherwise. Substituting 127.0.0.1 here would override a server policy
        // from the wrong side.
        let checked = form(TunnelKind::Remote).check().expect("valid");
        assert_eq!(checked.listen_host, "");
    }

    #[test]
    fn a_dynamic_forward_has_no_destination() {
        let checked = form(TunnelKind::Dynamic).check().expect("valid");
        assert_eq!(checked.plan, TunnelPlan::Dynamic);
        assert_eq!(checked.plan.target(), None);
        assert_eq!(checked.plan.kind(), TunnelKind::Dynamic);
    }

    #[test]
    fn the_two_directed_kinds_keep_their_destination_unresolved() {
        // Resolved by the server, not here: `localhost` in a local forward means the server's
        // loopback, and resolving it on this side would silently point it at this machine.
        for kind in [TunnelKind::Local, TunnelKind::Remote] {
            let mut f = form(kind);
            f.target_host = "localhost".to_string();
            let checked = f.check().expect("valid");
            assert_eq!(checked.plan.kind(), kind);
            assert_eq!(
                checked.plan.target(),
                Some(&("localhost".to_string(), 5432))
            );
        }
    }

    #[test]
    fn a_form_with_nothing_chosen_says_so_before_anything_else() {
        let mut f = form(TunnelKind::Local);
        f.over = None;
        assert_eq!(f.check(), Err(FormError::NoConnection));
    }

    #[test]
    fn a_port_that_is_not_a_port_is_refused() {
        let mut f = form(TunnelKind::Local);
        f.listen_port = "eight thousand".to_string();
        assert_eq!(f.check(), Err(FormError::ListenPort));

        let mut f = form(TunnelKind::Local);
        f.listen_port = "70000".to_string();
        assert_eq!(f.check(), Err(FormError::ListenPort));

        let mut f = form(TunnelKind::Local);
        f.target_port = "0".to_string();
        assert_eq!(
            f.check(),
            Err(FormError::TargetPort),
            "there is no port 0 to connect to"
        );
    }

    #[test]
    fn a_local_forward_may_ask_for_any_free_port_but_a_remote_one_may_not() {
        // Asymmetric because the two report back differently: a local forward hands over the address
        // it bound, and a remote one's allocated port is a number `RemoteForward` has no way to
        // update after the fact. Accepting 0 there would show the wrong port in the list.
        let mut f = form(TunnelKind::Local);
        f.listen_port = "0".to_string();
        assert!(f.check().is_ok());

        let mut f = form(TunnelKind::Remote);
        f.listen_port = "0".to_string();
        assert_eq!(f.check(), Err(FormError::RemotePortZero));
    }

    #[test]
    fn a_destination_is_required_where_there_is_one() {
        let mut f = form(TunnelKind::Local);
        f.target_host = "   ".to_string();
        assert_eq!(f.check(), Err(FormError::TargetHost));

        // ...and is not even looked at where there is not.
        let mut f = form(TunnelKind::Dynamic);
        f.target_host = String::new();
        f.target_port = String::new();
        assert!(f.check().is_ok());
    }

    #[test]
    fn every_kind_says_which_way_it_runs() {
        // The three are easy to confuse and the summaries are what stop somebody opening a hole in
        // the wrong direction, so each must actually be distinct.
        let summaries: Vec<_> = TunnelKind::ALL.iter().map(|k| k.summary()).collect();
        assert_eq!(summaries.len(), 3);
        assert_ne!(summaries[0], summaries[1]);
        assert_ne!(summaries[1], summaries[2]);
        assert!(summaries[2].contains("SOCKS"), "{}", summaries[2]);
    }
}
