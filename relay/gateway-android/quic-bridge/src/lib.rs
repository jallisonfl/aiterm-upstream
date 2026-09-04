use aiterm_relay_protocol::{
    DirectCookie, DirectId, DirectPacket, DIRECT_COOKIE_BYTES, DIRECT_ID_BYTES,
    MAX_DIRECT_PACKET_BYTES,
};
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::jlongArray;
use jni::JNIEnv;
use quinn::{ClientConfig, Endpoint, EndpointConfig, TokioRuntime, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);
const ALPN: &[u8] = b"aiterm-direct-tunnel/2";

struct Tunnel {
    shutdown: oneshot::Sender<()>,
}

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
static TUNNELS: OnceLock<Mutex<HashMap<i64, Tunnel>>> = OnceLock::new();
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn runtime() -> Result<&'static Runtime, String> {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .thread_name("aiterm-quic")
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn tunnels() -> &'static Mutex<HashMap<i64, Tunnel>> {
    TUNNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[no_mangle]
pub extern "system" fn Java_com_adroited_aiterm_remote_NativeDirectTunnel_startNative(
    mut env: JNIEnv,
    _class: JClass,
    host: JString,
    port: i32,
    id: JByteArray,
    cookie: JByteArray,
    fingerprint: JByteArray,
    server_name: JString,
) -> jlongArray {
    let result: Result<jlongArray, String> = (|| {
        let host: String = env
            .get_string(&host)
            .map_err(|_| "invalid relay host".to_string())?
            .into();
        let server_name: String = env
            .get_string(&server_name)
            .map_err(|_| "invalid desktop server name".to_string())?
            .into();
        let port = u16::try_from(port)
            .ok()
            .filter(|value| *value != 0)
            .ok_or_else(|| "invalid relay UDP port".to_string())?;
        let id = fixed_java_array::<DIRECT_ID_BYTES>(&env, &id, "rendezvous id")?;
        let cookie = fixed_java_array::<DIRECT_COOKIE_BYTES>(&env, &cookie, "rendezvous cookie")?;
        let fingerprint = fixed_java_array::<32>(&env, &fingerprint, "desktop fingerprint")?;
        let started = runtime()?.block_on(start_tunnel(
            host,
            port,
            id,
            cookie,
            fingerprint,
            server_name,
        ))?;
        let handle = next_handle();
        tunnels()
            .lock()
            .map_err(|_| "direct tunnel state failed".to_string())?
            .insert(
                handle,
                Tunnel {
                    shutdown: started.shutdown,
                },
            );
        let result = env
            .new_long_array(2)
            .map_err(|_| "could not return direct tunnel details".to_string())?;
        env.set_long_array_region(&result, 0, &[handle, i64::from(started.port)])
            .map_err(|_| "could not return direct tunnel details".to_string())?;
        Ok(result.into_raw())
    })();
    match result {
        Ok(value) => value,
        Err(error) => {
            let _ = env.throw_new("java/io/IOException", error);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_adroited_aiterm_remote_NativeDirectTunnel_stopNative(
    _env: JNIEnv,
    _class: JClass,
    handle: i64,
) {
    if let Ok(mut tunnels) = tunnels().lock() {
        if let Some(tunnel) = tunnels.remove(&handle) {
            let _ = tunnel.shutdown.send(());
        }
    }
}

fn fixed_java_array<const N: usize>(
    env: &JNIEnv,
    value: &JByteArray,
    label: &str,
) -> Result<[u8; N], String> {
    env.convert_byte_array(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("invalid {label}"))
}

fn next_handle() -> i64 {
    loop {
        let value = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        if value > 0 {
            return value;
        }
    }
}

struct StartedTunnel {
    port: u16,
    shutdown: oneshot::Sender<()>,
}

async fn start_tunnel(
    host: String,
    port: u16,
    id: DirectId,
    cookie: DirectCookie,
    fingerprint: [u8; 32],
    server_name: String,
) -> Result<StartedTunnel, String> {
    let relay = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| "relay direct endpoint could not be resolved".to_string())?
        .next()
        .ok_or_else(|| "relay direct endpoint could not be resolved".to_string())?;
    let bind_address = match relay {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind_address)
        .await
        .map_err(|_| "could not open a direct UDP socket".to_string())?;
    let peer = join_rendezvous(&socket, relay, id, cookie).await?;
    let probe = DirectPacket::Probe { id }.encode();
    for _ in 0..3 {
        let _ = socket.send_to(&probe, peer).await;
    }
    let std_socket = socket
        .into_std()
        .map_err(|_| "could not activate the direct UDP socket".to_string())?;
    let mut endpoint = Endpoint::new(
        EndpointConfig::default(),
        None,
        std_socket,
        Arc::new(TokioRuntime),
    )
    .map_err(|_| "could not start direct QUIC".to_string())?;
    endpoint.set_default_client_config(direct_client_config(fingerprint)?);
    let connection = connect_quic(&endpoint, peer, &server_name).await?;
    let (mut to_desktop, from_desktop) = connection
        .open_bi()
        .await
        .map_err(|_| "direct QUIC tunnel could not open".to_string())?;
    to_desktop
        .write_all(&cookie)
        .await
        .map_err(|_| "direct QUIC rendezvous proof could not be sent".to_string())?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "could not open the local direct proxy".to_string())?;
    let local_port = listener
        .local_addr()
        .map_err(|_| "could not inspect the local direct proxy".to_string())?
        .port();
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    runtime()?.spawn(async move {
        let accepted = tokio::select! {
            result = tokio::time::timeout(LOCAL_ACCEPT_TIMEOUT, listener.accept()) => {
                match result { Ok(Ok((socket, _))) => Some(socket), _ => None }
            }
            _ = &mut shutdown_rx => None,
        };
        if let Some(local) = accepted {
            let (mut local_read, mut local_write) = local.into_split();
            let mut to_desktop = to_desktop;
            let mut from_desktop = from_desktop;
            tokio::select! {
                _ = async {
                    let toward_desktop = tokio::io::copy(&mut local_read, &mut to_desktop);
                    let toward_phone = tokio::io::copy(&mut from_desktop, &mut local_write);
                    let _ = tokio::try_join!(toward_desktop, toward_phone);
                } => {}
                _ = &mut shutdown_rx => {}
            }
            let _ = to_desktop.finish();
        }
        connection.close(VarInt::from_u32(0), b"tunnel closed");
        endpoint.close(VarInt::from_u32(0), b"tunnel closed");
        endpoint.wait_idle().await;
    });
    Ok(StartedTunnel {
        port: local_port,
        shutdown,
    })
}

async fn connect_quic(
    endpoint: &Endpoint,
    address: SocketAddr,
    server_name: &str,
) -> Result<quinn::Connection, String> {
    let connecting = endpoint
        .connect(address, server_name)
        .map_err(|_| "QUIC connection could not start".to_string())?;
    tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| "QUIC connection timed out".to_string())?
        .map_err(|_| "QUIC handshake failed".to_string())
}

async fn join_rendezvous(
    socket: &UdpSocket,
    relay: SocketAddr,
    id: DirectId,
    cookie: DirectCookie,
) -> Result<SocketAddr, String> {
    let binding = DirectPacket::BindPhone { id, cookie }.encode();
    let deadline = tokio::time::Instant::now() + RENDEZVOUS_TIMEOUT;
    let mut bytes = [0u8; MAX_DIRECT_PACKET_BYTES];
    loop {
        socket
            .send_to(&binding, relay)
            .await
            .map_err(|_| "relay direct endpoint could not be reached".to_string())?;
        let slice_deadline =
            (tokio::time::Instant::now() + Duration::from_millis(500)).min(deadline);
        loop {
            let remaining = slice_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, socket.recv_from(&mut bytes)).await {
                Ok(Ok((count, source))) if source == relay => {
                    if let Ok(DirectPacket::Peer {
                        id: packet_id,
                        address,
                    }) = DirectPacket::decode(&bytes[..count])
                    {
                        if packet_id == id {
                            return Ok(address);
                        }
                    }
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => return Err("direct UDP socket failed".into()),
                Err(_) => break,
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("desktop did not join the direct rendezvous".into());
        }
    }
}

fn direct_client_config(fingerprint: [u8; 32]) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedVerifier {
        fingerprint,
        provider: provider.clone(),
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| "could not configure direct QUIC TLS".to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|_| "could not configure direct QUIC".to_string())?;
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(VarInt::from_u32(1));
    transport.max_concurrent_uni_streams(VarInt::from_u32(0));
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    // The relay path can have a lower effective MTU than either measured leg.
    // The QUIC minimum is deliberately conservative for cellular and VPN links.
    transport.mtu_discovery_config(None);
    let mut config = ClientConfig::new(Arc::new(quic));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

struct PinnedVerifier {
    fingerprint: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl fmt::Debug for PinnedVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedVerifier")
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let (_, certificate) =
            x509_parser::parse_x509_certificate(end_entity.as_ref()).map_err(|_| {
                rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
            })?;
        let actual: [u8; 32] = Sha256::digest(certificate.tbs_certificate.subject_pki.raw).into();
        if actual != self.fingerprint {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
