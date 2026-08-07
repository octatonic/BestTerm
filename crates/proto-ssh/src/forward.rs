//! Port forwarding, in all three directions.
//!
//! * A [`LocalForward`] (`ssh -L`) listens here and carries each connection to the far end. It is the
//!   feature people reach for to get at a database that only the bastion can see, and it is why the
//!   tunnel manager exists in every client of this kind.
//! * A [`RemoteForward`] (`ssh -R`) asks the *server* to listen and carries what arrives back to a
//!   local address. The direction is reversed, and with it the flow of control: the server opens the
//!   channels, so they arrive through the connection handler rather than through a call.
//! * A [`DynamicForward`] (`ssh -D`) is a SOCKS5 proxy whose destination is chosen per connection by
//!   the client, so one forward serves a whole browser session instead of one port.
//!
//! # Lifetime
//!
//! All three stop when dropped. That is deliberate: a forward that outlived the object representing
//! it would leave a port bound with no way to find or close it, and the next attempt to open the same
//! forward would fail with "address in use" for no visible reason.
//!
//! A remote forward is the one case where dropping cannot say everything, because telling the server
//! to stop listening means sending a request and `Drop` cannot wait for one. Dropping stops delivery
//! immediately, so anything that connects afterwards is refused; [`RemoteForward::close`] also
//! releases the port on the server. Ending the session releases it either way.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::socks;
use crate::transport::{SshConnection, SshError};

/// A connection the server opened to us, and the unanswered request that goes with it.
///
/// The two travel together because the answer depends on something the session's event loop must not
/// wait for: whether the local end can be reached.
pub(crate) struct Incoming {
    /// The channel, usable once `reply` has accepted.
    pub(crate) channel: russh::Channel<russh::client::Msg>,
    /// Accepts or refuses the server's request. Dropping it refuses.
    pub(crate) reply: russh::client::ChannelOpenHandle,
}

/// Routes server-opened channels to the remote forward that asked for them.
///
/// Keyed by port alone, deliberately. The server echoes back the bind address it was given, and the
/// spellings in play — `""`, `"0.0.0.0"`, `"localhost"`, `"::"` — do not compare equal even when they
/// mean the same interface. Two forwards cannot share a port anyway, so the port is already a
/// complete key and matching on the address could only ever lose a connection.
#[derive(Clone, Default)]
pub(crate) struct ForwardRegistry {
    ports: Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<Incoming>>>>,
}

impl ForwardRegistry {
    /// Claim `port`, returning where its connections will arrive.
    ///
    /// Replaces any previous claim: the server has one listener per port, so a second claim means the
    /// first is gone.
    fn register(&self, port: u16) -> mpsc::UnboundedReceiver<Incoming> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.locked().insert(u32::from(port), sender);
        receiver
    }

    /// Give up a port. Connections that arrive afterwards are refused.
    fn deregister(&self, port: u16) {
        self.locked().remove(&u32::from(port));
    }

    /// Where connections for `port` should go, if anything is listening.
    pub(crate) fn sink(&self, port: u32) -> Option<mpsc::UnboundedSender<Incoming>> {
        self.locked().get(&port).cloned()
    }

    /// The map, recovering from a poisoned lock rather than propagating the panic.
    ///
    /// Nothing here can be left half-updated by a panic — a `HashMap` insert either happened or did
    /// not — so poisoning carries no information, and panicking in the session's event loop would
    /// take down a working connection over an unrelated failure somewhere else.
    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<u32, mpsc::UnboundedSender<Incoming>>> {
        self.ports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A listening local forward.
///
/// Dropping it stops the listener and closes nothing that is already open: connections in flight
/// finish on their own, which is what `ssh -L` does when the session ends.
#[derive(Debug)]
pub struct LocalForward {
    local_addr: SocketAddr,
    target: String,
    task: tokio::task::JoinHandle<()>,
}

impl LocalForward {
    /// The address actually bound.
    ///
    /// Worth asking for even when a port was requested: binding port 0 lets the operating system
    /// choose, which is how a caller opens a forward without guessing what is free.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Where connections are carried to, as `host:port`.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Stop listening.
    pub fn stop(self) {
        // Dropping does this; the method exists so the intent can be written down at a call site.
    }
}

impl Drop for LocalForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SshConnection {
    /// Listen on `bind_address:bind_port` and carry each connection to `target_host:target_port`.
    ///
    /// Port `0` lets the operating system pick; read it back from [`LocalForward::local_addr`].
    ///
    /// Takes `&Arc<Self>` because the listener outlives the call and needs to keep the connection
    /// alive — a forward whose session had been dropped would accept connections and then fail every
    /// one of them.
    pub async fn open_local_forward(
        self: &Arc<Self>,
        bind_address: &str,
        bind_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Result<LocalForward, SshError> {
        let listener = TcpListener::bind((bind_address, bind_port)).await?;
        let local_addr = listener.local_addr()?;

        let target_host = target_host.into();
        let target = format!("{target_host}:{target_port}");
        let connection = Arc::clone(self);
        let host_for_task = target_host.clone();

        let task = tokio::spawn(async move {
            loop {
                let (mut socket, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::debug!(%error, "local forward stopped accepting");
                        return;
                    }
                };

                let connection = Arc::clone(&connection);
                let host = host_for_task.clone();

                // One task per connection: a slow or stuck peer must not stop the listener from
                // serving the next one.
                tokio::spawn(async move {
                    let channel = connection
                        .open_direct_tcpip(host, target_port, peer.ip().to_string(), peer.port())
                        .await;

                    match channel {
                        Ok(channel) => {
                            let mut stream = channel.into_stream();
                            // Ends when either side closes, which is the whole protocol of a
                            // forwarded connection.
                            if let Err(error) =
                                tokio::io::copy_bidirectional(&mut socket, &mut stream).await
                            {
                                tracing::debug!(%error, "forwarded connection ended");
                            }
                        }
                        Err(error) => {
                            // The far end refusing is normal — nothing may be listening there — and
                            // is reported per connection rather than taking the forward down.
                            tracing::debug!(%error, "the server refused a forwarded connection");
                        }
                    }
                });
            }
        });

        tracing::info!(%local_addr, %target, "opened a local forward");

        Ok(LocalForward {
            local_addr,
            target,
            task,
        })
    }

    /// Ask the server to listen on `bind_address:bind_port` and send what arrives to
    /// `target_host:target_port` here.
    ///
    /// Port `0` asks the server to choose; read it back from [`RemoteForward::remote_port`].
    ///
    /// Whether the server will bind anything other than its own loopback is its decision, not ours:
    /// `GatewayPorts no` is the OpenSSH default, and a request it will not honour comes back as
    /// [`SshError::ForwardDenied`].
    pub async fn open_remote_forward(
        self: &Arc<Self>,
        bind_address: &str,
        bind_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Result<RemoteForward, SshError> {
        let (remote_port, mut incoming) = if bind_port == 0 {
            // Nothing to claim until the server has chosen a port. It cannot accept a connection
            // before it has answered, and nothing else can learn the port before this returns, so
            // the gap between the answer and the claim has nobody in it.
            let port = self.request_remote_forward(bind_address, 0).await?;
            let incoming = self.forwards().register(port);
            (port, incoming)
        } else {
            // Claim first, so a connection cannot arrive before there is somewhere to put it.
            let incoming = self.forwards().register(bind_port);
            match self.request_remote_forward(bind_address, bind_port).await {
                Ok(port) => (port, incoming),
                Err(error) => {
                    self.forwards().deregister(bind_port);
                    return Err(error);
                }
            }
        };

        let target_host = target_host.into();
        let target = format!("{target_host}:{target_port}");
        let target_for_task = target.clone();

        let task = tokio::spawn(async move {
            while let Some(Incoming { channel, reply }) = incoming.recv().await {
                let target = target_for_task.clone();

                // One task per connection, for the same reason as a local forward: a stuck peer must
                // not hold up the next connection.
                tokio::spawn(async move {
                    // Connect before answering. The server's question is "can you take this?", and
                    // `ssh -R` answers it truthfully rather than accepting and hanging up, which the
                    // far end would see as the service breaking instead of not being there.
                    let mut socket = match TcpStream::connect(&target).await {
                        Ok(socket) => socket,
                        Err(error) => {
                            tracing::debug!(
                                %error,
                                %target,
                                "refused a forwarded connection: nothing is listening locally"
                            );
                            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
                            return;
                        }
                    };

                    reply.accept().await;
                    let mut stream = channel.into_stream();
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut socket, &mut stream).await
                    {
                        tracing::debug!(%error, "a remotely forwarded connection ended");
                    }
                });
            }
        });

        tracing::info!(
            bind_address,
            remote_port,
            %target,
            "opened a remote forward"
        );

        Ok(RemoteForward {
            bind_address: bind_address.to_string(),
            remote_port,
            target,
            connection: Arc::clone(self),
            task,
        })
    }

    /// Listen on `bind_address:bind_port` as a SOCKS5 proxy, sending each connection out from the
    /// server.
    ///
    /// Port `0` lets the operating system pick; read it back from [`DynamicForward::local_addr`].
    ///
    /// Names are resolved by the server, never here — that is the point of a dynamic forward, and
    /// resolving locally would both leak which names are being visited and answer in the wrong
    /// network.
    pub async fn open_dynamic_forward(
        self: &Arc<Self>,
        bind_address: &str,
        bind_port: u16,
    ) -> Result<DynamicForward, SshError> {
        let listener = TcpListener::bind((bind_address, bind_port)).await?;
        let local_addr = listener.local_addr()?;
        let connection = Arc::clone(self);

        let task = tokio::spawn(async move {
            loop {
                let (socket, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::debug!(%error, "dynamic forward stopped accepting");
                        return;
                    }
                };

                let connection = Arc::clone(&connection);
                tokio::spawn(serve_socks(connection, socket, peer));
            }
        });

        tracing::info!(%local_addr, "opened a dynamic forward");

        Ok(DynamicForward { local_addr, task })
    }
}

/// A remote forward: the server listens, and connections arrive here.
#[derive(Debug)]
pub struct RemoteForward {
    bind_address: String,
    remote_port: u16,
    target: String,
    /// Held so the session outlives the forward, and so the port can be released on the server.
    connection: Arc<SshConnection>,
    task: tokio::task::JoinHandle<()>,
}

impl RemoteForward {
    /// The port the server is listening on.
    ///
    /// Worth asking for even when a port was requested: port 0 lets the server choose, which is how a
    /// caller opens a remote forward without knowing what is free over there.
    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    /// The address the server was asked to bind.
    pub fn bind_address(&self) -> &str {
        &self.bind_address
    }

    /// Where connections are delivered on this machine, as `host:port`.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Stop, and tell the server to release the port.
    ///
    /// Worth preferring over dropping when the same port will be asked for again: dropping stops
    /// delivery at once, but the server's listener stays until the session ends, and asking for the
    /// port a second time would be refused.
    pub async fn close(self) -> Result<(), SshError> {
        // `Drop` still runs after this and stops delivery, whatever the server answers.
        self.connection
            .cancel_remote_forward(&self.bind_address, self.remote_port)
            .await
    }
}

impl Drop for RemoteForward {
    fn drop(&mut self) {
        self.connection.forwards().deregister(self.remote_port);
        self.task.abort();
    }
}

/// A dynamic forward: a SOCKS5 proxy on this machine, going out from the server.
#[derive(Debug)]
pub struct DynamicForward {
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl DynamicForward {
    /// The address the proxy is listening on — what to put in a browser's proxy settings.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop listening.
    pub fn stop(self) {
        // Dropping does this; the method exists so the intent can be written down at a call site.
    }
}

impl Drop for DynamicForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Take one connection through the SOCKS5 exchange and then carry it.
async fn serve_socks(connection: Arc<SshConnection>, mut socket: TcpStream, peer: SocketAddr) {
    let request = match socks::handshake(&mut socket).await {
        Ok(request) => request,
        Err(error) => {
            // Some failures happen before a reply would mean anything; `reply()` says which.
            if let Some(reply) = error.reply() {
                let _ = socks::send_reply(&mut socket, reply).await;
            }
            tracing::debug!(%error, %peer, "a socks client was turned away");
            return;
        }
    };

    let channel = connection
        .open_direct_tcpip(
            request.host.clone(),
            request.port,
            peer.ip().to_string(),
            peer.port(),
        )
        .await;

    let channel = match channel {
        Ok(channel) => channel,
        Err(error) => {
            // Nothing listening at the far end is the ordinary case, not a fault in the proxy.
            tracing::debug!(
                %error,
                host = %request.host,
                port = request.port,
                "the server could not reach a socks destination"
            );
            let _ = socks::send_reply(&mut socket, socks::Reply::ConnectionRefused).await;
            return;
        }
    };

    // Answered only once the far end is actually open, so a client that gets a success knows it can
    // start talking.
    if socks::send_reply(&mut socket, socks::Reply::Success)
        .await
        .is_err()
    {
        return;
    }

    let mut stream = channel.into_stream();
    if let Err(error) = tokio::io::copy_bidirectional(&mut socket, &mut stream).await {
        tracing::debug!(%error, "a proxied connection ended");
    }
}
