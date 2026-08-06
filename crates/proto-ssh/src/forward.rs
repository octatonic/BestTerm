//! Port forwarding.
//!
//! A local forward listens on this machine and carries each connection to the far end over the SSH
//! connection. It is the feature people reach for to get at a database that only the bastion can
//! see, and it is why the tunnel manager exists in every client of this kind.
//!
//! # Lifetime
//!
//! A [`LocalForward`] stops listening when it is dropped. That is deliberate: a forward that outlived
//! the object representing it would leave a port bound with no way to find or close it, and the next
//! attempt to open the same forward would fail with "address in use" for no visible reason.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::transport::{SshConnection, SshError};

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
                        .open_direct_tcpip(
                            host,
                            target_port,
                            peer.ip().to_string(),
                            peer.port(),
                        )
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
}
