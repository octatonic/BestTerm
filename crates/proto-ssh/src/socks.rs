//! Just enough SOCKS5 to be a dynamic forward.
//!
//! `ssh -D` is a SOCKS proxy that happens to carry its traffic over SSH. Only the CONNECT command is
//! implemented: BIND and UDP ASSOCIATE ask the proxy to listen on the client's behalf, which a
//! dynamic forward does not do and which every browser and `curl` gets along without.
//!
//! Authentication is "none". A proxy on loopback that asked for a password would be security
//! theatre — anything that can reach it can already read the process's memory — and every client
//! would need configuring for it.
//!
//! The wire format is parsed by pure functions so the fiddly parts, mostly length-prefixed
//! addresses, are testable without a socket.

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// SOCKS protocol version 5.
const VERSION: u8 = 0x05;

/// The only command implemented.
const CMD_CONNECT: u8 = 0x01;

/// "No authentication required".
const AUTH_NONE: u8 = 0x00;

/// "No acceptable methods".
const AUTH_UNACCEPTABLE: u8 = 0xFF;

/// Address types.
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Reply codes, from RFC 1928.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The connection was made.
    Success = 0x00,
    /// Something went wrong that has no more specific code.
    GeneralFailure = 0x01,
    /// The proxy will not carry this.
    NotAllowed = 0x02,
    /// Nothing answered at the far end.
    ConnectionRefused = 0x05,
    /// The client asked for BIND or UDP ASSOCIATE.
    CommandNotSupported = 0x07,
    /// The client used an address type we do not parse.
    AddressNotSupported = 0x08,
}

/// What the client asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Host to connect to, as text — a domain is passed through unresolved.
    ///
    /// Resolution is deliberately left to the SSH server: that is the whole point of a dynamic
    /// forward. Resolving here would leak which names are being visited into local DNS and, worse,
    /// resolve them in the wrong network — the name only means something on the far side.
    pub host: String,
    /// Port to connect to.
    pub port: u16,
}

/// Why a SOCKS exchange failed.
#[derive(Debug, thiserror::Error)]
pub enum SocksError {
    /// The socket failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// Not SOCKS5.
    #[error("unsupported SOCKS version {0}")]
    Version(u8),

    /// The client offered no method we accept.
    #[error("the client offered no acceptable authentication method")]
    NoAcceptableAuth,

    /// BIND or UDP ASSOCIATE.
    #[error("unsupported SOCKS command {0}")]
    Command(u8),

    /// An address type we do not parse.
    #[error("unsupported SOCKS address type {0}")]
    AddressType(u8),

    /// A domain name that is not UTF-8.
    #[error("the target host name is not valid UTF-8")]
    HostNotUtf8,
}

impl SocksError {
    /// The reply code to send back before closing, if a reply would mean anything.
    ///
    /// A client that is told *why* can say something useful; one that just sees the socket close
    /// reports "proxy failed" and leaves the user with nowhere to look.
    ///
    /// `None` for the two failures that happen before the exchange reaches the point where a reply
    /// is defined: a client speaking another version would read the ten bytes as some other message
    /// entirely, and a client with no acceptable method has already been answered in the greeting.
    pub fn reply(&self) -> Option<Reply> {
        match self {
            Self::Command(_) => Some(Reply::CommandNotSupported),
            Self::AddressType(_) | Self::HostNotUtf8 => Some(Reply::AddressNotSupported),
            Self::Io(_) => Some(Reply::GeneralFailure),
            Self::Version(_) | Self::NoAcceptableAuth => None,
        }
    }
}

/// Perform the greeting and read the CONNECT request.
pub async fn handshake<S>(stream: &mut S) -> Result<Request, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Greeting: version, method count, then that many method bytes.
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != VERSION {
        return Err(SocksError::Version(head[0]));
    }

    let mut methods = vec![0u8; usize::from(head[1])];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&AUTH_NONE) {
        // Answer before closing, so the client knows this was a refusal rather than a broken proxy.
        stream.write_all(&[VERSION, AUTH_UNACCEPTABLE]).await?;
        return Err(SocksError::NoAcceptableAuth);
    }
    stream.write_all(&[VERSION, AUTH_NONE]).await?;

    // Request: version, command, reserved, address type.
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != VERSION {
        return Err(SocksError::Version(request[0]));
    }
    if request[1] != CMD_CONNECT {
        return Err(SocksError::Command(request[1]));
    }

    let host = match request[3] {
        ATYP_IPV4 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets).await?;
            std::net::Ipv4Addr::from(octets).to_string()
        }
        ATYP_IPV6 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            std::net::Ipv6Addr::from(octets).to_string()
        }
        ATYP_DOMAIN => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await?;
            let mut name = vec![0u8; usize::from(length[0])];
            stream.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| SocksError::HostNotUtf8)?
        }
        other => return Err(SocksError::AddressType(other)),
    };

    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;

    Ok(Request {
        host,
        port: u16::from_be_bytes(port),
    })
}

/// The bytes of a reply.
///
/// The bound address is always `0.0.0.0:0`. It is meant to say which local address the proxy used,
/// and no client checks it for a CONNECT; inventing something more truthful would mean asking the
/// SSH server a question it has no way to answer.
pub fn reply_bytes(reply: Reply) -> [u8; 10] {
    [VERSION, reply as u8, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0]
}

/// Send a reply.
pub async fn send_reply<S>(stream: &mut S, reply: Reply) -> Result<(), std::io::Error>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(&reply_bytes(reply)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplex stream over fixed input, collecting what is written.
    ///
    /// Reading past the end fills nothing, which is how a `ReadBuf` says "end of stream" — so a
    /// truncated exchange surfaces as `UnexpectedEof` rather than hanging the test.
    struct Fake {
        input: Vec<u8>,
        read: usize,
        output: Vec<u8>,
    }

    impl Fake {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input,
                read: 0,
                output: Vec::new(),
            }
        }
    }

    impl AsyncRead for Fake {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let take = (this.input.len() - this.read).min(buf.remaining());
            buf.put_slice(&this.input[this.read..this.read + take]);
            this.read += take;
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for Fake {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.output.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn greeting() -> Vec<u8> {
        vec![0x05, 0x01, 0x00]
    }

    #[tokio::test]
    async fn a_domain_request_is_passed_through_unresolved() {
        // The point of a dynamic forward: the name is resolved on the far side, where it means
        // something, and never appears in local DNS.
        let mut input = greeting();
        input.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, 11]);
        input.extend_from_slice(b"example.int");
        input.extend_from_slice(&443u16.to_be_bytes());

        let mut stream = Fake::new(input);
        let request = handshake(&mut stream).await.expect("handshake");
        assert_eq!(request.host, "example.int");
        assert_eq!(request.port, 443);
        // The method selection was answered.
        assert_eq!(&stream.output, &[0x05, 0x00]);
    }

    #[tokio::test]
    async fn an_ipv4_request_is_read() {
        let mut input = greeting();
        input.extend_from_slice(&[0x05, 0x01, 0x00, 0x01, 10, 0, 0, 5]);
        input.extend_from_slice(&5432u16.to_be_bytes());

        let request = handshake(&mut Fake::new(input)).await.expect("handshake");
        assert_eq!(request.host, "10.0.0.5");
        assert_eq!(request.port, 5432);
    }

    #[tokio::test]
    async fn an_ipv6_request_is_read() {
        let mut input = greeting();
        input.extend_from_slice(&[0x05, 0x01, 0x00, 0x04]);
        input.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        input.extend_from_slice(&22u16.to_be_bytes());

        let request = handshake(&mut Fake::new(input)).await.expect("handshake");
        assert_eq!(request.host, "2001:db8::1");
        assert_eq!(request.port, 22);
    }

    #[tokio::test]
    async fn a_wrong_version_is_refused() {
        let error = handshake(&mut Fake::new(vec![0x04, 0x01, 0x00]))
            .await
            .expect_err("SOCKS4 is not supported");
        assert!(matches!(error, SocksError::Version(4)), "got {error:?}");
    }

    #[tokio::test]
    async fn a_client_offering_only_authenticated_methods_is_told_so() {
        // 0x02 is username/password. Answering before closing is what lets the client report
        // something better than "the proxy hung up".
        let mut stream = Fake::new(vec![0x05, 0x01, 0x02]);
        let error = handshake(&mut stream).await.expect_err("no shared method");
        assert!(matches!(error, SocksError::NoAcceptableAuth));
        assert_eq!(&stream.output, &[0x05, 0xFF]);
    }

    #[tokio::test]
    async fn bind_and_udp_are_refused_by_name() {
        for command in [0x02u8, 0x03] {
            let mut input = greeting();
            input.extend_from_slice(&[0x05, command, 0x00, 0x01, 127, 0, 0, 1, 0, 80]);
            let error = handshake(&mut Fake::new(input))
                .await
                .expect_err("only CONNECT is implemented");
            assert!(matches!(error, SocksError::Command(_)), "got {error:?}");
            assert_eq!(error.reply(), Some(Reply::CommandNotSupported));
        }
    }

    #[tokio::test]
    async fn an_unknown_address_type_is_refused() {
        let mut input = greeting();
        input.extend_from_slice(&[0x05, 0x01, 0x00, 0x09]);
        let error = handshake(&mut Fake::new(input)).await.expect_err("bad atyp");
        assert!(matches!(error, SocksError::AddressType(9)));
        assert_eq!(error.reply(), Some(Reply::AddressNotSupported));
    }

    #[tokio::test]
    async fn the_failures_before_a_reply_is_defined_do_not_get_one() {
        // Ten bytes of SOCKS5 sent to a SOCKS4 client would be read as some other message; the
        // no-method case was already answered inside the greeting.
        let version = handshake(&mut Fake::new(vec![0x04, 0x01, 0x00]))
            .await
            .expect_err("wrong version");
        assert_eq!(version.reply(), None);

        let no_method = handshake(&mut Fake::new(vec![0x05, 0x01, 0x02]))
            .await
            .expect_err("no shared method");
        assert_eq!(no_method.reply(), None);
    }

    #[tokio::test]
    async fn a_domain_that_is_not_utf8_is_refused_rather_than_mangled() {
        let mut input = greeting();
        input.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, 2, 0xFF, 0xFE]);
        input.extend_from_slice(&80u16.to_be_bytes());
        let error = handshake(&mut Fake::new(input)).await.expect_err("bad name");
        assert!(matches!(error, SocksError::HostNotUtf8));
    }

    #[test]
    fn a_reply_is_ten_bytes_with_the_code_in_the_second() {
        let success = reply_bytes(Reply::Success);
        assert_eq!(success.len(), 10);
        assert_eq!(success[0], 0x05);
        assert_eq!(success[1], 0x00);
        assert_eq!(success[3], ATYP_IPV4);

        assert_eq!(reply_bytes(Reply::ConnectionRefused)[1], 0x05);
    }
}
