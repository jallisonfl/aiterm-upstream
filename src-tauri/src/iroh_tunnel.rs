//! iroh transport for remote access — the same API, reachable from anywhere.
//!
//! The TLS listener in `remote` serves the phone perfectly once a packet can
//! reach it, and that is the part the world keeps breaking: no UPnP at the
//! office, client isolation on the Wi‑Fi, CGNAT on cellular. iroh solves
//! exactly that — the desktop holds a keypair, the phone dials the public
//! key, and iroh finds a path (direct when it can hole-punch, a blind relay
//! when it cannot; either way the QUIC layer is encrypted end to end).
//!
//! This module deliberately adds no second protocol. Each incoming iroh
//! bi-stream is copied byte-for-byte to a TCP connection to our own TLS
//! listener on localhost, so the phone speaks the exact HTTPS it already
//! speaks — same certificate pinning, same token — through whatever path
//! iroh found. The relay sees TLS inside QUIC and can read neither.

use iroh::endpoint::{presets, Connection, Endpoint, PortmapperConfig, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{RelayMode, RelayUrl, SecretKey};

pub const ALPN: &[u8] = b"aiterm/remote/0";

/// The running tunnel. Dropping the router closes the endpoint.
pub struct Tunnel {
    router: Router,
    pub node_id: String,
}

/// Parse the stored secret, or mint one. 32 bytes, hex — same shape as the
/// bearer token, so `remote.json` stays one small readable file.
pub fn secret_from_hex(hex: &str) -> Option<SecretKey> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        bytes[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(SecretKey::from_bytes(&bytes))
}

pub fn new_secret_hex() -> String {
    let sk = SecretKey::generate();
    sk.to_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// The node id a phone dials: the public half of the stored secret. Derived,
/// so the QR can carry it whether or not the endpoint is up yet.
pub fn node_id_of(secret_hex: &str) -> Option<String> {
    Some(secret_from_hex(secret_hex)?.public().to_string())
}

#[derive(Debug, Clone)]
struct Forward {
    port: u16,
}

impl ProtocolHandler for Forward {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = format!("{}", conn.remote_id());
        crate::diag!("remote", "iroh peer connected: {peer}");
        // One bi-stream per TCP connection the phone would have made; QUIC
        // multiplexes them on the one path.
        loop {
            match conn.accept_bi().await {
                Ok((send, recv)) => {
                    let port = self.port;
                    tokio::spawn(pump(send, recv, port));
                }
                Err(_) => {
                    crate::diag!("remote", "iroh peer gone: {peer}");
                    return Ok(());
                }
            }
        }
    }
}

/// Copy bytes both ways between one iroh stream pair and one local TCP
/// connection to the TLS listener. Ends when either side does.
async fn pump(mut send: SendStream, mut recv: RecvStream, port: u16) {
    let tcp = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(t) => t,
        Err(e) => {
            crate::diag!("remote", "iroh stream had no listener to reach: {e}");
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_write).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut tcp_write).await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut send).await;
        let _ = send.finish();
    };
    tokio::join!(up, down);
}

/// Bind the endpoint and start forwarding to the local listener. With a
/// `relay_url` the endpoint uses that relay alone instead of n0's — for a
/// person who runs their own iroh-relay and wants no third party even for
/// the fallback path. `portmap` is the router port-mapping setting: false
/// (the default) keeps iroh from probing the gateway with UPnP, PCP or
/// NAT-PMP; hole-punching through the relay still works without it.
pub async fn start(secret: SecretKey, port: u16, relay_url: Option<String>, portmap: bool) -> Result<Tunnel, String> {
    let portmapper = if portmap { PortmapperConfig::default() } else { PortmapperConfig::Disabled };
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .portmapper_config(portmapper);
    if let Some(url) = relay_url {
        let url: RelayUrl = url.parse().map_err(|e| format!("iroh relay URL {url:?} is invalid: {e}"))?;
        builder = builder.relay_mode(RelayMode::custom([url]));
    }
    let endpoint = builder
        .bind()
        .await
        .map_err(|e| format!("iroh bind failed: {e}"))?;
    let node_id = format!("{}", endpoint.id());
    let router = Router::builder(endpoint).accept(ALPN, Forward { port }).spawn();
    crate::diag!("remote", "iroh listening as {node_id}");
    Ok(Tunnel { router, node_id })
}

pub async fn stop(tunnel: Tunnel) {
    let _ = tunnel.router.shutdown().await;
}
