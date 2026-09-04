//! Remote access — a phone as a second client of the session model.
//!
//! Off by default. When on, the desktop listens on one port for plain HTTP
//! and one WebSocket, behind a bearer token the phone learns from a QR. The
//! phone sees exactly what the sidebar sees (`list_sessions`), reads a
//! session as the conversation `session_conversation` already assembles for
//! every backend, and sends a line of input into the tab that is running it.
//! When it asks to open or start a session, the desktop opens the tab — so
//! both screens always agree about what is running.
//!
//! What this deliberately is not: a terminal. The phone never receives PTY
//! bytes and never owns one.
//!
//! Reaching the desktop from outside the house is the desktop's own job —
//! there is no relay and no third party in the path. While remote access is
//! on, the desktop asks the router (UPnP IGD) to map its port, learns its
//! public address, and puts LAN and public addresses in the QR; the phone
//! tries them in order. The listener is TLS with a self-signed identity the
//! desktop mints once and keeps; the QR carries the certificate's SHA-256
//! and the phone trusts that certificate and nothing else. The token is what
//! stops a stranger who can reach the port; repeated bad tokens from one
//! address are refused for a while.
//!
//! State on disk is one file, `remote.json` in the aiterm data directory,
//! owner-readable only. Turning remote access off keeps the token, so the
//! phone reconnects without a new QR; "forget phones" rotates it.

use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;

use crate::remote::relay::{RelayConfig, RelayConnectionState, RelayConnectorHandle, RelayEnrollmentDraft};
use crate::remote_roads::{self, DraftSlot, DraftStep, VpnStatus};

const DEFAULT_PORT: u16 = 8877;
/// Bumped when a phone would misread an older desktop. The phone checks it.
const API_VERSION: u32 = 1;

// ---------------------------------------------------------------- config

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
    /// What the phone shows for this machine. The hostname, unless edited.
    pub name: String,
    /// iroh identity, 32 bytes hex. The public half is the address a phone
    /// dials from anywhere; minted on the first start after this existed.
    #[serde(default)]
    pub iroh_secret: String,
    /// Whether the iroh tunnel rides alongside the listener. On by default —
    /// it is what makes the desktop reachable off-LAN with no port forward —
    /// but a person who wants LAN/VPN only can turn just this off.
    #[serde(default = "default_true")]
    pub iroh_enabled: bool,
    /// The direct roads (`docs/remote/remote-roads.md`): private addresses on the
    /// same network, and tunnel addresses (Tailscale, WireGuard…). Each
    /// decides whether its addresses go in the QR and in `/v1/status`.
    #[serde(default = "default_true")]
    pub lan_enabled: bool,
    #[serde(default = "default_true")]
    pub vpn_enabled: bool,
    /// The AITerm relay road. Off by default; it carries nothing until a
    /// phone enrolls a route (`phone-relay.json`), which the next QR offers.
    #[serde(default)]
    pub relay_enabled: bool,
    /// A custom iroh relay URL. None = iroh's default (n0) relays.
    #[serde(default)]
    pub iroh_relay_url: Option<String>,
    /// The order phones try the roads, most preferred first — published in
    /// `/v1/status` and adopted by any phone that has not set its own.
    /// Always a permutation of the four (`load_config` makes it one).
    #[serde(default = "remote_roads::default_road_order")]
    pub road_order: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            port: DEFAULT_PORT,
            token: new_token(),
            name: hostname(),
            iroh_secret: crate::iroh_tunnel::new_secret_hex(),
            iroh_enabled: true,
            lan_enabled: true,
            vpn_enabled: true,
            relay_enabled: false,
            iroh_relay_url: None,
            road_order: remote_roads::default_road_order(),
        }
    }
}

/// The phone listener's relay route, beside the gateway's `relay.json`.
const PHONE_RELAY_FILE: &str = "phone-relay.json";

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "aiterm".into())
}

/// 32 bytes from the kernel, written as hex. Hex rather than base64 so the
/// token is safe in a URL query and a QR without any escaping to get wrong.
fn new_token() -> String {
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("aiterm").join("remote.json"))
}

fn load_config() -> Config {
    let mut cfg: Config = config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // A hand-edited or older file: unknown roads dropped, missing ones
    // appended, so the order is always whole.
    cfg.road_order = remote_roads::normalize_road_order(&cfg.road_order);
    cfg
}

fn save_config(cfg: &Config) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(cfg) else { return };
    if std::fs::write(&path, json).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

// ---------------------------------------------------------------- state

/// What the desktop pushes to every connected phone. The phone treats each
/// as "go and look again", not as data: the truth is always a GET away.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Transcripts changed on disk — the list, or a conversation, moved.
    SessionsChanged,
    /// A tab's process ended. `session_id` is the session the tab was bound
    /// to, when it was bound to one.
    SessionExit { session_id: Option<String>, code: Option<u32> },
    /// A session raised a desktop notification — it is waiting on a person.
    Attention { title: String, body: String },
    /// A tab's activity changed: "working" | "attention" | "idle".
    Activity { session_id: String, activity: String },
    /// A file in a session's workspace was created, modified or deleted.
    FileChanged { session_id: String, path: String, kind: String },
    /// A second-agent relay changed state; `session_id` is the first
    /// agent's session — where the phone is looking.
    Relay {
        session_id: String,
        b_session_id: Option<String>,
        b_name: String,
        phase: String,
        round: u32,
        rounds: u32,
        note: String,
    },
    /// The machine's name was edited; phones show the new one at once.
    Renamed { name: String },
    /// The desktop's roads changed — hosts, a relay route enrolled or
    /// cleared. The phone re-reads `/v1/status`.
    StatusChanged,
    /// One spine event, flattened: `{"type":"spine","seq":…,"kind":"agent_text",…}`.
    /// Never sent through `notify` — it rides its own broadcast channel
    /// (`spine::Spine::subscribe`) so that bootstrapping a long session
    /// cannot lag a phone out of the coarse events above.
    Spine(crate::spine::SpineEvent),
    Ping,
}

struct Running {
    port: u16,
    handle: axum_server::Handle<SocketAddr>,
    /// Set false to end the UPnP renewal loop.
    upnp_alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// What the router said, for the panel and the QR.
#[derive(Clone, Default)]
struct Reach {
    /// "off" | "searching" | "mapped" | "no_router" | "refused"
    upnp: String,
    public_ip: Option<IpAddr>,
}

/// A phone holding the event socket open — the definition of "connected".
#[derive(Clone, Serialize)]
pub struct ClientInfo {
    pub id: u64,
    /// What the phone calls itself ("Google Pixel 10 Pro XL").
    pub device: String,
    pub os: String,
    pub app: String,
    pub address: String,
    /// Unix seconds.
    pub since: u64,
}

/// The relay road's state: the enrolled route (persisted), its connector
/// while the listener runs, and the enrollment draft a phone signs.
///
/// The draft is eager (`ensure_draft`): whenever the road is on and no
/// route exists there is one waiting, minted at listener start, when the
/// road is switched on, after the route is cleared, and lazily on any
/// status read once it is older than `DRAFT_TTL` or a prepare failed. A
/// paired phone reads its digest from `/v1/status` and enrolls with no
/// new pairing; a QR carries the same digest as `ta`.
#[derive(Default)]
struct PhoneRelay {
    config: Option<RelayConfig>,
    handle: Option<RelayConnectorHandle>,
    /// In memory only, with when it was minted. One at a time: a
    /// re-prepare replaces it. Dropped when the road is off or a route lives.
    draft: Option<(RelayEnrollmentDraft, Instant)>,
    /// Why the last prepare failed (the relay server unreachable), and when.
    /// Cleared by the next successful prepare, by the road going off, and
    /// by the route going live.
    failed: Option<(String, Instant)>,
    /// A prepare is in flight; the next status read does not start another.
    preparing: bool,
}

impl PhoneRelay {
    fn slot(&self) -> DraftSlot {
        match (&self.draft, &self.failed) {
            (Some((_, at)), _) => DraftSlot::Waiting { age: at.elapsed() },
            (None, Some((_, at))) => DraftSlot::Failed { age: at.elapsed() },
            (None, None) => DraftSlot::Empty,
        }
    }
    fn digest(&self) -> Option<[u8; 32]> {
        self.draft.as_ref().map(|(d, _)| *d.authorization_digest())
    }
}

fn load_phone_relay() -> Option<RelayConfig> {
    let root = crate::remote::state_root().ok()?;
    match RelayConfig::load_named(&root, PHONE_RELAY_FILE) {
        Ok(c) => c,
        Err(e) => {
            crate::diag!("remote", "phone relay route ignored: {e}");
            None
        }
    }
}

pub struct RemoteState {
    config: Mutex<Config>,
    running: Mutex<Option<Running>>,
    /// The iroh endpoint while listening — the reach-from-anywhere path.
    tunnel: Mutex<Option<crate::iroh_tunnel::Tunnel>>,
    phone_relay: Mutex<PhoneRelay>,
    reach: Mutex<Reach>,
    clients: Mutex<HashMap<u64, ClientInfo>>,
    next_client: std::sync::atomic::AtomicU64,
    /// Last good answer per usage source. A service that rate-limits the
    /// question this minute still had a number a minute ago.
    usage_cache: Mutex<HashMap<String, crate::usage::UsageSource>>,
    /// Why the last start failed, for the settings panel. Cleared on success.
    last_error: Mutex<Option<String>>,
    /// Bad tokens per address: (failures, first failure). See `auth`.
    strikes: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// Provider model catalogs, cached — the phone asks on every connect
    /// and OpenRouter's /models is not a thing to curl that often.
    models_cache: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    /// Live preview tickets: unguessable path → what it serves. Minted by
    /// the authed API, honored without a bearer — a WebView can't attach
    /// headers to subresource requests, but it can keep a secret path.
    previews: Mutex<HashMap<String, Preview>>,
    events: broadcast::Sender<Event>,
}

#[derive(Clone)]
enum PreviewTarget {
    /// Reverse-proxy to a server on the desktop's loopback.
    Port(u16),
    /// Serve files out of a folder (an agent-built static page).
    Dir(PathBuf),
}

struct Preview {
    target: PreviewTarget,
    expires: Instant,
}

impl Default for RemoteState {
    fn default() -> Self {
        RemoteState {
            config: Mutex::new(load_config()),
            running: Mutex::new(None),
            tunnel: Mutex::new(None),
            phone_relay: Mutex::new(PhoneRelay { config: load_phone_relay(), ..Default::default() }),
            reach: Mutex::new(Reach { upnp: "off".into(), public_ip: None }),
            clients: Mutex::new(HashMap::new()),
            next_client: std::sync::atomic::AtomicU64::new(1),
            usage_cache: Mutex::new(HashMap::new()),
            last_error: Mutex::new(None),
            strikes: Mutex::new(HashMap::new()),
            models_cache: Mutex::new(HashMap::new()),
            previews: Mutex::new(HashMap::new()),
            events: broadcast::channel(64).0,
        }
    }
}

// ---------------------------------------------------------------- identity

/// The listener's certificate, minted once and kept beside the config. Its
/// SHA-256 is what the phone pins, so regenerating it means pairing again —
/// which is why it is only ever generated when the files are absent.
struct Identity {
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    fingerprint: String,
}

fn identity() -> Result<Identity, String> {
    let dir = config_path().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or("no data dir")?;
    let cert_path = dir.join("remote-cert.pem");
    let key_path = dir.join("remote-key.pem");
    let (cert_pem, key_pem) = match (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        (Ok(c), Ok(k)) => (c, k),
        _ => {
            let ck = rcgen::generate_simple_self_signed(vec!["aiterm".to_string()])
                .map_err(|e| format!("could not create a certificate: {e}"))?;
            let c = ck.cert.pem().into_bytes();
            let k = ck.signing_key.serialize_pem().into_bytes();
            let _ = std::fs::create_dir_all(&dir);
            std::fs::write(&cert_path, &c).map_err(|e| e.to_string())?;
            std::fs::write(&key_path, &k).map_err(|e| e.to_string())?;
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
            (c, k)
        }
    };
    let der = pem_to_der(&cert_pem).ok_or("certificate file is not PEM")?;
    use sha2::Digest;
    let fingerprint = sha2::Sha256::digest(&der).iter().map(|b| format!("{b:02x}")).collect();
    Ok(Identity { cert_pem, key_pem, fingerprint })
}

fn pem_to_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    base64_decode(&body)
}

/// Standard base64 (what PEM uses), decoded without a crate.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = T.iter().position(|&t| t == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- reachability

const UPNP_LEASE_SECS: u32 = 3600;

/// Ask the router for the port, and keep asking while we are on. Runs on
/// its own thread: IGD discovery is a blocking multicast search and the
/// renewal is a sleep loop. Every outcome is written to `reach` for the
/// panel; none of them is an error the listener cares about — a desktop
/// with no cooperative router still serves the LAN.
fn keep_port_mapped(app: AppHandle, port: u16, alive: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    let set = |upnp: &str, ip: Option<IpAddr>| {
        if let Some(state) = app.try_state::<RemoteState>() {
            *state.reach.lock().unwrap() = Reach { upnp: upnp.into(), public_ip: ip };
        }
    };
    set("searching", None);
    let options = igd_next::SearchOptions { timeout: Some(Duration::from_secs(4)), ..Default::default() };
    let gateway = match igd_next::search_gateway(options) {
        Ok(g) => g,
        Err(e) => {
            crate::diag!("remote", "no UPnP router: {e}");
            set("no_router", None);
            return;
        }
    };
    // The address the router can reach us on: whichever interface routes to it.
    let local_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect(gateway.addr).map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let local = SocketAddr::new(local_ip, port);
    let mut ip = gateway.get_external_ip().ok();
    while alive.load(Ordering::Relaxed) {
        match gateway.add_port(igd_next::PortMappingProtocol::TCP, port, local, UPNP_LEASE_SECS, "aiterm remote") {
            Ok(()) => {
                ip = gateway.get_external_ip().ok().or(ip);
                set("mapped", ip);
            }
            Err(e) => {
                crate::diag!("remote", "router refused the port mapping: {e}");
                set("refused", ip);
            }
        }
        // Renew well inside the lease; wake often enough that stop is prompt.
        let mut slept = 0;
        while alive.load(Ordering::Relaxed) && slept < UPNP_LEASE_SECS / 2 {
            std::thread::sleep(Duration::from_secs(5));
            slept += 5;
        }
    }
    let _ = gateway.remove_port(igd_next::PortMappingProtocol::TCP, port);
    set("off", None);
}

/// Push an event to every connected phone. Cheap and never fails: with no
/// phone listening the event is dropped, which is the right outcome.
pub fn notify(app: &AppHandle, event: Event) {
    if let Some(state) = app.try_state::<RemoteState>() {
        let _ = state.events.send(event);
    }
}

/// Called once at startup: resume listening if it was on last time.
pub fn autostart(app: &AppHandle) {
    let enabled = app.state::<RemoteState>().config.lock().unwrap().enabled;
    if enabled {
        if let Err(e) = start(app) {
            crate::diag!("remote", "not listening: {e}");
        }
    }
}

fn start(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<RemoteState>();
    if state.running.lock().unwrap().is_some() {
        return Ok(());
    }
    let port = state.config.lock().unwrap().port;
    // Bound synchronously so "port in use" is an error the panel shows,
    // not a log line inside a task nobody reads.
    let std_listener = std::net::TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| format!("could not listen on port {port}: {e}"))?;
    // Tokio adopts the socket and requires it non-blocking; handing it a
    // blocking one panics the accept loop, silently, on a worker thread.
    std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let id = identity()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let handle = axum_server::Handle::<SocketAddr>::new();
    let router = router(app.clone());
    let served = handle.clone();
    tauri::async_runtime::spawn(async move {
        let tls = match axum_server::tls_rustls::RustlsConfig::from_pem(id.cert_pem, id.key_pem).await {
            Ok(t) => t,
            Err(e) => {
                crate::diag!("remote", "TLS setup failed: {e}");
                return;
            }
        };
        let server = match axum_server::from_tcp_rustls(std_listener, tls) {
            Ok(s) => s,
            Err(e) => {
                crate::diag!("remote", "listener handoff failed: {e}");
                return;
            }
        };
        let r = server
            .handle(served)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await;
        if let Err(e) = r {
            crate::diag!("remote", "listener ended: {e}");
        }
    });
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let app = app.clone();
        let alive = alive.clone();
        std::thread::spawn(move || keep_port_mapped(app, port, alive));
    }
    // The iroh endpoint rides alongside: a config without a key yet (created
    // before this existed) gets one now, so its node id is stable from here on.
    let (secret, relay_url, relay_road) = {
        let mut cfg = state.config.lock().unwrap();
        if crate::iroh_tunnel::secret_from_hex(&cfg.iroh_secret).is_none() {
            cfg.iroh_secret = crate::iroh_tunnel::new_secret_hex();
            save_config(&cfg);
        }
        let secret = if cfg.iroh_enabled {
            crate::iroh_tunnel::secret_from_hex(&cfg.iroh_secret)
        } else {
            None
        };
        (secret, cfg.iroh_relay_url.clone(), cfg.relay_enabled)
    };
    if let Some(secret) = secret {
        spawn_tunnel(app, secret, port, relay_url);
    }
    // The relay road rides alongside too, when it is on and a route exists.
    if relay_road {
        start_phone_relay(state.inner(), port);
    }
    *state.running.lock().unwrap() = Some(Running { port, handle, upnp_alive: alive });
    *state.last_error.lock().unwrap() = None;
    crate::diag!("remote", "listening (TLS) on port {port}");
    // With the road on and no route, a draft waits from the first moment a
    // phone could read it.
    ensure_draft(app);
    Ok(())
}

fn stop(app: &AppHandle) {
    let state = app.state::<RemoteState>();
    let taken = state.running.lock().unwrap().take();
    if let Some(running) = taken {
        running.upnp_alive.store(false, std::sync::atomic::Ordering::Relaxed);
        running.handle.graceful_shutdown(Some(Duration::from_secs(2)));
        state.clients.lock().unwrap().clear();
        let _ = app.emit("remote://clients", ());
        crate::diag!("remote", "stopped listening on port {}", running.port);
    }
    let tunnel = state.tunnel.lock().unwrap().take();
    if let Some(tunnel) = tunnel {
        tauri::async_runtime::spawn(crate::iroh_tunnel::stop(tunnel));
    }
    stop_phone_relay(state.inner());
}

/// Bind the iroh endpoint on the runtime and keep it in state once it is up.
fn spawn_tunnel(app: &AppHandle, secret: iroh::SecretKey, port: u16, relay_url: Option<String>) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        match crate::iroh_tunnel::start(secret, port, relay_url).await {
            Ok(t) => *app2.state::<RemoteState>().tunnel.lock().unwrap() = Some(t),
            Err(e) => crate::diag!("remote", "{e}"),
        }
    });
}

/// Start the relay connector for the enrolled phone route, if there is one
/// and it is not already running. `RelayConnectorHandle::start` spawns on
/// tokio, so it is called from inside the runtime whatever thread we are on.
fn start_phone_relay(state: &RemoteState, port: u16) {
    let mut relay = state.phone_relay.lock().unwrap();
    if relay.handle.is_some() {
        return;
    }
    let Some(config) = relay.config.clone() else { return };
    let local = SocketAddr::from(([127, 0, 0, 1], port));
    let runtime = tauri::async_runtime::handle();
    let _enter = runtime.inner().enter();
    relay.handle = Some(RelayConnectorHandle::start(config, local));
    crate::diag!("remote", "phone relay connector started for {}:{}", relay.config.as_ref().map(|c| c.public_host.as_str()).unwrap_or(""), relay.config.as_ref().map(|c| c.public_port).unwrap_or(0));
}

fn stop_phone_relay(state: &RemoteState) {
    let handle = state.phone_relay.lock().unwrap().handle.take();
    if let Some(handle) = handle {
        tauri::async_runtime::spawn(handle.stop());
    }
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub name: String,
    /// Addresses a phone might reach this machine on, best first.
    pub addresses: Vec<String>,
    /// What the router said: "off" | "searching" | "mapped" | "no_router" | "refused".
    pub upnp: String,
    /// The address the internet sees, when the router told us.
    pub public_address: Option<String>,
    /// SHA-256 of the listener certificate, hex — what a paired phone pins.
    pub fingerprint: Option<String>,
    /// Phones holding the event socket open right now.
    pub clients: Vec<ClientInfo>,
    pub error: Option<String>,
    /// Whether the iroh tunnel is configured to ride alongside the listener.
    pub iroh_enabled: bool,
    /// The reach-from-anywhere address: this desktop's iroh node id.
    pub iroh_node: Option<String>,
    /// Which roads are on. See `docs/remote/remote-roads.md`.
    pub roads: Roads,
    /// What the VPN road sees on this machine right now.
    pub vpn: VpnStatus,
    /// The relay road: enrolled route, connector state, pending draft.
    pub relay: PhoneRelayStatus,
    /// Custom iroh relay URL; None = iroh's default relays.
    pub iroh_relay_url: Option<String>,
    /// The order phones try the roads; what `/v1/status` publishes.
    pub road_order: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct Roads {
    pub lan: bool,
    pub vpn: bool,
    pub relay: bool,
    pub iroh: bool,
}

impl Roads {
    fn of(cfg: &Config) -> Roads {
        Roads { lan: cfg.lan_enabled, vpn: cfg.vpn_enabled, relay: cfg.relay_enabled, iroh: cfg.iroh_enabled }
    }
}

#[derive(Clone, Serialize)]
pub struct PhoneRelayStatus {
    pub configured: bool,
    /// "off" | "connecting" | "connected" | "retrying"
    pub state: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub server: String,
    /// A draft is waiting for a phone to sign it (no route yet).
    pub pending_enrollment: bool,
    /// The relay server could not be reached when a draft was wanted.
    /// `pending_enrollment` is then false; the next status read after
    /// `DRAFT_RETRY` asks again.
    pub error: Option<String>,
}

fn phone_relay_status(state: &RemoteState) -> PhoneRelayStatus {
    let relay = state.phone_relay.lock().unwrap();
    let conn = relay.handle.as_ref().map(RelayConnectorHandle::state).unwrap_or_default();
    PhoneRelayStatus {
        configured: relay.config.is_some(),
        state: match conn {
            RelayConnectionState::Off => "off",
            RelayConnectionState::Connecting => "connecting",
            RelayConnectionState::Connected => "connected",
            RelayConnectionState::Retrying => "retrying",
        }
        .into(),
        host: relay.config.as_ref().map(|c| c.public_host.clone()),
        port: relay.config.as_ref().map(|c| c.public_port),
        server: crate::remote::DEFAULT_RELAY_SERVER.into(),
        pending_enrollment: relay.config.is_none() && relay.draft.is_some(),
        error: relay.failed.as_ref().map(|(e, _)| e.clone()),
    }
}

fn status_of(app: &AppHandle) -> RemoteStatus {
    ensure_draft(app);
    let state = app.state::<RemoteState>();
    let cfg = state.config.lock().unwrap().clone();
    let running = state.running.lock().unwrap().is_some();
    let error = state.last_error.lock().unwrap().clone();
    let reach = state.reach.lock().unwrap().clone();
    let mut clients: Vec<ClientInfo> = state.clients.lock().unwrap().values().cloned().collect();
    clients.sort_by_key(|c| c.since);
    RemoteStatus {
        enabled: cfg.enabled,
        running,
        port: cfg.port,
        addresses: addresses(&cfg),
        upnp: reach.upnp,
        public_address: reach.public_ip.map(|ip| ip.to_string()),
        fingerprint: identity().ok().map(|i| i.fingerprint),
        clients,
        error,
        iroh_enabled: cfg.iroh_enabled,
        iroh_node: crate::iroh_tunnel::node_id_of(&cfg.iroh_secret),
        roads: Roads::of(&cfg),
        vpn: remote_roads::vpn_status(),
        relay: phone_relay_status(&state),
        iroh_relay_url: cfg.iroh_relay_url.clone(),
        road_order: cfg.road_order.clone(),
        name: cfg.name,
    }
}

/// Change the port. Takes effect at once when listening: the listener and
/// the router mapping both move. Paired phones keep working only if they
/// scan again — the port is in the QR — so the panel says so.
#[tauri::command]
pub fn remote_set_port(app: AppHandle, port: u16) -> Result<RemoteStatus, String> {
    if port < 1024 {
        return Err("Pick a port from 1024 to 65535".into());
    }
    let was_running = {
        let state = app.state::<RemoteState>();
        let mut cfg = state.config.lock().unwrap();
        if cfg.port == port {
            return Ok(status_of(&app));
        }
        cfg.port = port;
        save_config(&cfg);
        let running = state.running.lock().unwrap().is_some();
        running
    };
    if was_running {
        stop(&app);
        // The old socket closes asynchronously; give it a moment before rebinding.
        std::thread::sleep(Duration::from_millis(300));
        if let Err(e) = start(&app) {
            *app.state::<RemoteState>().last_error.lock().unwrap() = Some(e.clone());
            return Err(e);
        }
    }
    Ok(status_of(&app))
}

/// IPv4 addresses on real interfaces, ordered so the one a phone is most
/// likely to share comes first: a VPN address (Tailscale's 100.64/10, a
/// `wg*` tunnel) beats the LAN, which beats anything else. A road that is
/// off contributes nothing. Loopback is never a candidate — a phone cannot
/// reach it. The rules live in `remote_roads`.
fn addresses(cfg: &Config) -> Vec<String> {
    remote_roads::advertised(cfg.lan_enabled, cfg.vpn_enabled)
}

/// The host list a QR and `/v1/status` carry: the roads' addresses, then
/// the router-reported public address — the port-forward path, which is
/// the LAN road seen from outside, so it follows `lan_enabled`.
fn hosts_of(state: &RemoteState, cfg: &Config) -> Vec<String> {
    let mut hosts = addresses(cfg);
    if cfg.lan_enabled {
        if let Some(ip) = state.reach.lock().unwrap().public_ip {
            hosts.push(ip.to_string());
        }
    }
    hosts
}

#[tauri::command]
pub fn remote_api_status(app: AppHandle) -> RemoteStatus {
    status_of(&app)
}

#[tauri::command]
pub fn remote_set_enabled(app: AppHandle, on: bool) -> RemoteStatus {
    {
        let state = app.state::<RemoteState>();
        let mut cfg = state.config.lock().unwrap();
        cfg.enabled = on;
        save_config(&cfg);
    }
    if on {
        if let Err(e) = start(&app) {
            *app.state::<RemoteState>().last_error.lock().unwrap() = Some(e);
        }
    } else {
        stop(&app);
    }
    status_of(&app)
}

/// Forget every phone: a new token, and every open connection dropped by
/// the next request it makes. Pairing again is a new QR.
#[tauri::command]
pub fn remote_rotate_token(app: AppHandle) -> RemoteStatus {
    let state = app.state::<RemoteState>();
    let mut cfg = state.config.lock().unwrap();
    cfg.token = new_token();
    save_config(&cfg);
    drop(cfg);
    status_of(&app)
}

/// Turn the iroh tunnel on or off without touching the listener. Live: a
/// running listener gains or loses its tunnel at once. Phones keep their
/// LAN route either way; only the reach-from-anywhere path changes.
#[tauri::command]
pub fn remote_set_iroh(app: AppHandle, on: bool) -> RemoteStatus {
    let _ = set_road(&app, "iroh", on);
    status_of(&app)
}

/// Turn one road on or off, live. `lan`/`vpn` change what the QR and
/// `/v1/status` advertise; `relay` starts or stops the connector for an
/// enrolled route, and with no route prepares an enrollment draft that any
/// paired phone signs from `/v1/status`; `iroh` is `remote_set_iroh`.
#[tauri::command]
pub fn remote_set_road(app: AppHandle, road: String, on: bool) -> Result<RemoteStatus, String> {
    set_road(&app, &road, on)?;
    Ok(status_of(&app))
}

fn set_road(app: &AppHandle, road: &str, on: bool) -> Result<(), String> {
    let state = app.state::<RemoteState>();
    let (changed, port, secret, relay_url) = {
        let mut cfg = state.config.lock().unwrap();
        let field = match road {
            "lan" => &mut cfg.lan_enabled,
            "vpn" => &mut cfg.vpn_enabled,
            "relay" => &mut cfg.relay_enabled,
            "iroh" => &mut cfg.iroh_enabled,
            _ => return Err(format!("no such road: {road}")),
        };
        let changed = *field != on;
        *field = on;
        if changed {
            save_config(&cfg);
        }
        (changed, cfg.port, crate::iroh_tunnel::secret_from_hex(&cfg.iroh_secret), cfg.iroh_relay_url.clone())
    };
    if !changed {
        return Ok(());
    }
    let running = state.running.lock().unwrap().is_some();
    match road {
        "iroh" if running => {
            if on {
                if let Some(secret) = secret {
                    spawn_tunnel(app, secret, port, relay_url);
                }
            } else if let Some(tunnel) = state.tunnel.lock().unwrap().take() {
                tauri::async_runtime::spawn(crate::iroh_tunnel::stop(tunnel));
            }
        }
        "relay" => {
            if on {
                if running {
                    start_phone_relay(&state, port);
                }
            } else {
                stop_phone_relay(&state);
            }
            // On with no route: a draft is prepared now. Off: the draft and
            // any error go.
            ensure_draft(app);
        }
        _ => {}
    }
    // Phones re-read their host list; the panel re-reads status.
    notify(app, Event::StatusChanged);
    let _ = app.emit("remote://clients", ());
    Ok(())
}

/// Point iroh at a relay of one's own (or back at the default with None).
/// A running tunnel restarts on the new relay; the node id does not change.
#[tauri::command]
pub fn remote_set_iroh_relay_url(app: AppHandle, url: Option<String>) -> Result<RemoteStatus, String> {
    let url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    if let Some(u) = &url {
        u.parse::<iroh::RelayUrl>().map_err(|e| format!("Not a relay URL: {e}"))?;
    }
    let state = app.state::<RemoteState>();
    let (changed, port, secret) = {
        let mut cfg = state.config.lock().unwrap();
        let changed = cfg.iroh_relay_url != url;
        cfg.iroh_relay_url = url.clone();
        if changed {
            save_config(&cfg);
        }
        let secret = if cfg.iroh_enabled { crate::iroh_tunnel::secret_from_hex(&cfg.iroh_secret) } else { None };
        (changed, cfg.port, secret)
    };
    let running = state.running.lock().unwrap().is_some();
    if changed && running {
        if let Some(secret) = secret {
            let old = state.tunnel.lock().unwrap().take();
            let app2 = app.clone();
            // Stop before start: the same key must not be bound twice.
            tauri::async_runtime::spawn(async move {
                if let Some(old) = old {
                    crate::iroh_tunnel::stop(old).await;
                }
                match crate::iroh_tunnel::start(secret, port, url).await {
                    Ok(t) => *app2.state::<RemoteState>().tunnel.lock().unwrap() = Some(t),
                    Err(e) => crate::diag!("remote", "{e}"),
                }
            });
        }
    }
    Ok(status_of(&app))
}

/// Forget the phone relay route: release it at the relay server when it
/// was provisioned by pairing, delete `phone-relay.json`, stop the
/// connector. The road's on/off setting is untouched — a fresh draft is
/// prepared at once, so the next phone to read status enrolls a new route.
#[tauri::command]
pub async fn remote_phone_relay_clear(app: AppHandle) -> Result<RemoteStatus, String> {
    let state = app.state::<RemoteState>();
    let (config, handle) = {
        let mut relay = state.phone_relay.lock().unwrap();
        relay.draft = None;
        relay.failed = None;
        (relay.config.clone(), relay.handle.take())
    };
    if let Some(handle) = handle {
        handle.stop().await;
    }
    let removal = match &config {
        Some(config) => config.deprovision().await,
        None => Ok(()),
    };
    if let Err(e) = removal {
        // The route stays; put its connector back so state matches disk.
        let (port, relay_road) = {
            let cfg = state.config.lock().unwrap();
            (cfg.port, cfg.relay_enabled)
        };
        if relay_road && state.running.lock().unwrap().is_some() {
            start_phone_relay(&state, port);
        }
        return Err(e);
    }
    let root = crate::remote::state_root()?;
    match std::fs::remove_file(root.join(PHONE_RELAY_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("could not remove the phone relay route: {e}")),
    }
    state.phone_relay.lock().unwrap().config = None;
    notify(&app, Event::StatusChanged);
    let _ = app.emit("remote://clients", ());
    // The road is still on: the next phone to read status enrolls afresh.
    ensure_draft(&app);
    Ok(status_of(&app))
}

/// The order phones try the roads. Must name all four, each once; phones
/// that have not set their own order adopt it on their next status read.
#[tauri::command]
pub fn remote_set_road_order(app: AppHandle, order: Vec<String>) -> Result<RemoteStatus, String> {
    if !remote_roads::is_road_permutation(&order) {
        return Err(format!("The order must name each of {} once", remote_roads::ROADS.join(", ")));
    }
    let state = app.state::<RemoteState>();
    let changed = {
        let mut cfg = state.config.lock().unwrap();
        let changed = cfg.road_order != order;
        if changed {
            cfg.road_order = order;
            save_config(&cfg);
        }
        changed
    };
    if changed {
        notify(&app, Event::StatusChanged);
    }
    Ok(status_of(&app))
}

#[tauri::command]
pub fn remote_set_name(app: AppHandle, name: String) -> RemoteStatus {
    let state = app.state::<RemoteState>();
    let mut cfg = state.config.lock().unwrap();
    let name = name.trim();
    cfg.name = if name.is_empty() { hostname() } else { name.to_string() };
    save_config(&cfg);
    let renamed = cfg.name.clone();
    drop(cfg);
    notify(&app, Event::Renamed { name: renamed });
    status_of(&app)
}

#[derive(Serialize)]
pub struct PairPayload {
    pub uri: String,
    pub svg: String,
}

/// The QR is one URI and nothing else:
/// `aiterm://pair?v=1&p=<port>&t=<token>&f=<cert sha256>&n=<name>&h=<addr>&h=<addr>…`
/// `h` repeats, LAN addresses first and the public one last; the phone
/// tries them in order and keeps the one that answers. `f` is the listener
/// certificate the phone will trust and nothing else. The token is the only
/// secret and this is the only place it leaves the desktop.
#[tauri::command]
pub async fn remote_pair_payload(app: AppHandle) -> Result<PairPayload, String> {
    let state = app.state::<RemoteState>();
    if state.running.lock().unwrap().is_none() {
        return Err("Turn remote access on first".into());
    }
    ensure_draft_now(&app).await;
    let cfg = state.config.lock().unwrap().clone();
    let addrs = hosts_of(&state, &cfg);
    let (relay, digest) = phone_relay_fields(&state, &cfg);
    if addrs.is_empty() && relay.is_none() && !cfg.iroh_enabled {
        return Err("No network address a phone could reach".into());
    }
    let fingerprint = identity()?.fingerprint;
    let mut uri = format!(
        "aiterm://pair?v={API_VERSION}&p={}&t={}&f={}&n={}",
        cfg.port,
        cfg.token,
        fingerprint,
        percent_encode(&cfg.name)
    );
    for h in &addrs {
        uri.push_str("&h=");
        uri.push_str(h);
    }
    // The reach-from-anywhere address: the iroh node id. A phone that knows
    // it can dial this desktop with no address at all.
    if cfg.iroh_enabled {
        if let Some(id) = crate::iroh_tunnel::node_id_of(&cfg.iroh_secret) {
            uri.push_str("&z=");
            uri.push_str(&id);
        }
    }
    // The relay road, under the same names the combined QR uses.
    if let Some((host, port)) = &relay {
        uri.push_str("&tr=");
        uri.push_str(&percent_encode(host));
        uri.push_str(&format!("&tq={port}"));
    }
    if let Some(digest) = &digest {
        uri.push_str("&ta=");
        uri.push_str(&remote_roads::b64url(digest));
    }
    let code = qrcode::QrCode::new(uri.as_bytes()).map_err(|e| e.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    Ok(PairPayload { uri, svg })
}

/// The phone-listener's fields for a combined pairing QR, namespaced so they
/// ride beside the gateway's own (`p`/`f`/`s`/`h` belong to the gateway
/// there): `&tp=<port>&tt=<token>&tf=<cert sha256>[&th=<host>…][&z=<iroh
/// node id>][&tr=<relay host>&tq=<relay port>][&ta=<digest>]`. `None` while
/// the listener is off — a combined QR then simply carries no phone-listener
/// route. The token leaves the desktop only inside a rendered QR, same as in
/// `remote_pair_payload`. Async because a relay draft is awaited here when
/// the relay road is on and none is waiting yet.
pub(crate) async fn pair_extension(app: &AppHandle) -> Option<String> {
    let state = app.try_state::<RemoteState>()?;
    if state.running.lock().unwrap().is_none() {
        return None;
    }
    ensure_draft_now(app).await;
    let cfg = state.config.lock().unwrap().clone();
    let fingerprint = identity().ok()?.fingerprint;
    let hosts = hosts_of(&state, &cfg);
    let iroh = if cfg.iroh_enabled { crate::iroh_tunnel::node_id_of(&cfg.iroh_secret) } else { None };
    let (relay, digest) = phone_relay_fields(&state, &cfg);
    Some(remote_roads::pair_fields(
        cfg.port,
        &cfg.token,
        &fingerprint,
        &hosts,
        iroh.as_deref(),
        relay.as_ref().map(|(h, p)| (h.as_str(), *p)),
        digest.as_ref(),
    ))
}

/// What the QR says about the relay road: the live route, or the draft's
/// route plus the digest the phone must sign. Nothing when the road is off.
fn phone_relay_fields(state: &RemoteState, cfg: &Config) -> (Option<(String, u16)>, Option<[u8; 32]>) {
    if !cfg.relay_enabled {
        return (None, None);
    }
    let relay = state.phone_relay.lock().unwrap();
    if let Some(c) = &relay.config {
        return (Some((c.public_host.clone(), c.public_port)), None);
    }
    if let Some((d, _)) = &relay.draft {
        let (host, port) = d.public_endpoint();
        return (Some((host.to_string(), port)), Some(*d.authorization_digest()));
    }
    (None, None)
}

/// One step of the draft state machine (`remote_roads::draft_step`) under
/// the locks: what the slot needs right now. `Prepare` marks the prepare
/// in flight so a second read does not start another; the caller runs it.
fn draft_decision(state: &RemoteState) -> DraftStep {
    let cfg = state.config.lock().unwrap();
    let mut relay = state.phone_relay.lock().unwrap();
    let step = remote_roads::draft_step(cfg.relay_enabled, relay.config.is_some(), relay.preparing, relay.slot());
    match step {
        DraftStep::Prepare => relay.preparing = true,
        DraftStep::Drop => {
            relay.draft = None;
            relay.failed = None;
        }
        DraftStep::Keep => {}
    }
    step
}

/// Keep a draft waiting whenever the road is on and no route exists — from
/// listener start, a road switch, a cleared route, and every status read.
/// Never waits: a needed prepare is spawned and its outcome arrives as a
/// status change. A dropped draft is a status change too.
fn ensure_draft(app: &AppHandle) {
    match draft_decision(&app.state::<RemoteState>()) {
        DraftStep::Prepare => {
            tauri::async_runtime::spawn(prepare_phone_relay_draft(app.clone()));
        }
        DraftStep::Drop => {
            notify(app, Event::StatusChanged);
            let _ = app.emit("remote://clients", ());
        }
        DraftStep::Keep => {}
    }
}

/// The QR's version: a needed prepare is awaited so the code can carry
/// `ta`. A prepare another read already has in flight is not waited for —
/// the QR then carries no digest and the phone enrolls from status once
/// it is paired, which is the ordinary path anyway.
async fn ensure_draft_now(app: &AppHandle) {
    match draft_decision(&app.state::<RemoteState>()) {
        DraftStep::Prepare => prepare_phone_relay_draft(app.clone()).await,
        DraftStep::Drop => {
            notify(app, Event::StatusChanged);
            let _ = app.emit("remote://clients", ());
        }
        DraftStep::Keep => {}
    }
}

/// Ask the relay server for a route (`GET /v1/info` on Matt's control
/// origin) and keep the draft. Runs only after `draft_decision` said
/// `Prepare`, and clears the in-flight mark when done. The answer is
/// discarded if the road went off or a route went live meanwhile. Success
/// replaces whatever draft was there and tells phones; failure keeps the
/// message for the panel and `/v1/status`, with no draft.
async fn prepare_phone_relay_draft(app: AppHandle) {
    let outcome = match identity().ok().and_then(|id| remote_roads::hex_to_b64url(&id.fingerprint)) {
        Some(spki) => RelayConfig::prepare_enrollment(crate::remote::DEFAULT_RELAY_SERVER, &spki).await,
        None => Err("the listener has no certificate yet".into()),
    };
    let state = app.state::<RemoteState>();
    let landed = {
        let cfg = state.config.lock().unwrap();
        let mut relay = state.phone_relay.lock().unwrap();
        relay.preparing = false;
        if !cfg.relay_enabled || relay.config.is_some() {
            relay.draft = None;
            relay.failed = None;
            None
        } else {
            match outcome {
                Ok(draft) => {
                    relay.draft = Some((draft, Instant::now()));
                    relay.failed = None;
                    Some(true)
                }
                Err(e) => {
                    relay.draft = None;
                    relay.failed = Some((e, Instant::now()));
                    Some(false)
                }
            }
        }
    };
    match landed {
        Some(true) => {
            crate::diag!("remote", "phone relay enrollment draft ready; a paired phone signs it from status");
            notify(&app, Event::StatusChanged);
        }
        Some(false) => crate::diag!(
            "remote",
            "relay setup was unavailable: {}",
            state.phone_relay.lock().unwrap().failed.as_ref().map(|(e, _)| e.as_str()).unwrap_or("")
        ),
        None => {}
    }
    let _ = app.emit("remote://clients", ());
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- server

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
}

fn router(app: AppHandle) -> Router {
    let ctx = Ctx { app };
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/usage", get(usage))
        .route("/v1/agents", get(agents))
        .route("/v1/uploads", post(upload).layer(axum::extract::DefaultBodyLimit::max(UPLOAD_LIMIT + 1024)))
        .route("/v1/search", get(search))
        .route("/v1/files", get(file))
        .route("/v1/sessions", get(sessions).post(new_session))
        .route("/v1/tabs", get(tabs))
        .route("/v1/sessions/{id}/artifacts", get(artifacts))
        .route("/v1/sessions/{id}/files", get(session_files))
        .route("/v1/sessions/{id}/changes", get(session_changes))
        .route("/v1/browse", get(browse))
        .route("/v1/dirs", post(make_dir))
        .route("/v1/sessions/{id}", get(detail))
        .route("/v1/sessions/{id}/conversation", get(conversation))
        .route("/v1/sessions/{id}/spine", get(spine))
        .route("/v1/sessions/{id}/open", post(open))
        .route("/v1/sessions/{id}/input", post(input))
        .route("/v1/sessions/{id}/rename", post(rename))
        .route("/v1/sessions/{id}/star", post(star))
        .route("/v1/sessions/{id}/bringin", post(bring_in))
        .route("/v1/sessions/{id}/interrupt", post(interrupt))
        .route("/v1/sessions/{id}/stop", post(stop_session))
        .route("/v1/terminal", post(terminal_open))
        .route("/v1/terminal/{tab}/screen", get(terminal_screen))
        .route("/v1/terminal/{tab}/input", post(terminal_input))
        .route("/v1/terminal/{tab}/close", post(terminal_close))
        .route("/v1/events", get(events))
        .route("/v1/previews", post(make_preview))
        .route("/v1/relay/enroll", post(relay_enroll))
        .layer(middleware::from_fn_with_state(ctx.clone(), auth))
        // Below the auth layer on purpose: preview paths carry their own
        // unguessable ticket. See `make_preview`.
        .route("/p/{ticket}/", get(preview_root))
        .route("/p/{ticket}/{*rest}", get(preview_rest))
        // Gzip on everything the phone reads: /v1/sessions alone is 185 KB
        // raw and 27 KB compressed [measured 2026-08-31], and every byte
        // rides a ~300ms relay path when the office Wi-Fi isolates clients.
        // OkHttp asks for gzip by itself and decompresses transparently.
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(ctx)
}

// ------------------------------------------------------------- previews
//
// "The agent built a web page — show me." Two shapes: a folder of static
// files (the usual index.html landing page), or a dev server the agent
// started on the desktop's loopback. Either way the phone can't reach it
// directly, and a WebView can't send Authorization headers for the page's
// images and stylesheets — so the authed API mints a ticket, and the
// ticket IS the credential: an unguessable path prefix, expiring, serving
// exactly one target.

const PREVIEW_TTL: Duration = Duration::from_secs(3600);

#[derive(Deserialize)]
struct PreviewBody {
    port: Option<u16>,
    dir: Option<String>,
}

async fn make_preview(State(ctx): State<Ctx>, Json(b): Json<PreviewBody>) -> Response {
    let target = if let Some(p) = b.port {
        PreviewTarget::Port(p)
    } else if let Some(d) = b.dir {
        let Ok(real) = std::path::PathBuf::from(&d).canonicalize() else {
            return err(StatusCode::NOT_FOUND, "no such folder");
        };
        // Home, plus claude's scratchpads — where throwaway demo pages
        // land by convention (/tmp/claude-<uid>/<project>/scratchpad).
        let scratch = real.to_string_lossy().starts_with("/tmp/claude-");
        if !real.is_dir() || !(under_home(&real) || scratch) {
            return err(StatusCode::FORBIDDEN, "only folders under home");
        }
        PreviewTarget::Dir(real)
    } else {
        return err(StatusCode::BAD_REQUEST, "port or dir required");
    };
    let ticket = new_token();
    let state = ctx.app.state::<RemoteState>();
    let mut p = state.previews.lock().unwrap();
    p.retain(|_, v| v.expires > Instant::now());
    p.insert(ticket.clone(), Preview { target, expires: Instant::now() + PREVIEW_TTL });
    Json(serde_json::json!({ "path": format!("/p/{ticket}/") })).into_response()
}

async fn preview_root(
    State(ctx): State<Ctx>,
    Path(ticket): Path<String>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    serve_preview(ctx, ticket, String::new(), q).await
}

async fn preview_rest(
    State(ctx): State<Ctx>,
    Path((ticket, rest)): Path<(String, String)>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    serve_preview(ctx, ticket, rest, q).await
}

async fn serve_preview(ctx: Ctx, ticket: String, rest: String, query: Option<String>) -> Response {
    let target = {
        let state = ctx.app.state::<RemoteState>();
        let p = state.previews.lock().unwrap();
        match p.get(&ticket) {
            Some(v) if v.expires > Instant::now() => v.target.clone(),
            _ => return err(StatusCode::NOT_FOUND, "preview expired — reopen it from the app"),
        }
    };
    match target {
        PreviewTarget::Dir(dir) => {
            if rest.contains("..") {
                return err(StatusCode::FORBIDDEN, "no");
            }
            let mut path = dir.join(rest.trim_start_matches('/'));
            if rest.is_empty() || path.is_dir() {
                path = path.join("index.html");
            }
            match std::fs::read(&path) {
                Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, mime_of(&path))], bytes).into_response(),
                Err(_) => err(StatusCode::NOT_FOUND, "not found"),
            }
        }
        PreviewTarget::Port(port) => proxy_local(port, &rest, query.as_deref()).await,
    }
}

/// One GET against a loopback server, HTTP/1.0 so the body is
/// close-delimited — no client crate, no chunked parsing. Dev servers
/// (python http.server, vite, uvicorn) all answer 1.0 happily.
async fn proxy_local(port: u16, rest: &str, query: Option<&str>) -> Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = format!(
        "/{}{}",
        rest.trim_start_matches('/'),
        query.map(|q| format!("?{q}")).unwrap_or_default()
    );
    let work = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.ok()?;
        Some(buf)
    };
    let Ok(Some(raw)) = tokio::time::timeout(Duration::from_secs(20), work).await else {
        return err(StatusCode::BAD_GATEWAY, format!("nothing answered on port {port}"));
    };
    // Split head from body, pull status and content-type, pass the rest on.
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0);
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = raw[(split + 4).min(raw.len())..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .and_then(|c| StatusCode::from_u16(c).ok())
        .unwrap_or(StatusCode::OK);
    let ctype = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))
        .map(|l| l[13..].trim().to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
    (status, [(axum::http::header::CONTENT_TYPE, ctype)], body).into_response()
}

fn mime_of(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Listening TCP ports owned by a process tree — how a session's dev
/// server is noticed. `ss` names the pid per listener; /proc names each
/// pid's parent; the intersection is "this session is serving something".
fn ports_of_tree(root: u32) -> Vec<u16> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            // ppid is the 2nd field after the parenthesised comm.
            let Some(after) = stat.rsplit(')').next() else { continue };
            let mut it = after.split_whitespace();
            let _state = it.next();
            if let Some(ppid) = it.next().and_then(|p| p.parse::<u32>().ok()) {
                kids.entry(ppid).or_default().push(pid);
            }
        }
    }
    let mut tree = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        if tree.insert(p) {
            if let Some(c) = kids.get(&p) {
                stack.extend(c);
            }
        }
    }
    let Ok(out) = std::process::Command::new("ss").args(["-ltnpH"]).output() else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut ports = Vec::new();
    for line in text.lines() {
        let Some(pid) = line.split("pid=").nth(1).and_then(|r| r.split(&[',', ')'][..]).next()).and_then(|p| p.parse::<u32>().ok()) else { continue };
        if !tree.contains(&pid) {
            continue;
        }
        // Local address is the 4th column; the port follows the last ':'.
        if let Some(addr) = line.split_whitespace().nth(3) {
            if let Some(port) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    ports.sort_unstable();
    ports
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

const STRIKES_ALLOWED: u32 = 20;
const STRIKES_WINDOW: Duration = Duration::from_secs(600);

/// Every route, the WebSocket included. The token is read per request so a
/// rotation takes effect on the next call, with nothing to restart. An
/// address that keeps presenting bad tokens is refused outright for a
/// while — the port may be reachable from the internet, and a 256-bit
/// token is only unguessable if guessing is slow.
async fn auth(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    req: Request,
    next: Next,
) -> Response {
    let state = ctx.app.state::<RemoteState>();
    {
        let mut strikes = state.strikes.lock().unwrap();
        if let Some((n, since)) = strikes.get(&peer.ip()).copied() {
            if since.elapsed() > STRIKES_WINDOW {
                strikes.remove(&peer.ip());
            } else if n >= STRIKES_ALLOWED {
                return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({ "error": "too many bad tokens" }))).into_response();
            }
        }
    }
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
        .or(q.token);
    let expected = state.config.lock().unwrap().token.clone();
    let ok = presented.is_some_and(|p| constant_eq(p.as_bytes(), expected.as_bytes()));
    if !ok {
        let mut strikes = state.strikes.lock().unwrap();
        let e = strikes.entry(peer.ip()).or_insert((0, Instant::now()));
        e.0 += 1;
        if e.0 == 1 || e.0 == STRIKES_ALLOWED {
            crate::diag!("remote", "bad token from {} ({} so far)", peer.ip(), e.0);
        }
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "unauthorized" }))).into_response();
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

async fn status(State(ctx): State<Ctx>) -> Response {
    // A phone reading status is the moment a draft is wanted: mint one
    // (spawned — the phone never waits on the relay server) or replace a
    // stale one; the phone hears the result as a status change.
    ensure_draft(&ctx.app);
    let state = ctx.app.state::<RemoteState>();
    let cfg = state.config.lock().unwrap().clone();
    // The addresses the desktop answers on right now, LAN first, public
    // last — the same list the QR carries. The phone refreshes its
    // candidates from this on every connect, so a DHCP move or a new
    // public IP never strands it with only stale addresses.
    let hosts = hosts_of(&state, &cfg);
    let (relay, enroll, relay_error) = {
        let r = state.phone_relay.lock().unwrap();
        if !cfg.relay_enabled {
            (None, None, None)
        } else {
            (
                r.config.as_ref().map(|c| (c.public_host.clone(), c.public_port)),
                if r.config.is_none() { r.digest() } else { None },
                r.failed.as_ref().map(|(e, _)| e.clone()),
            )
        }
    };
    Json(status_body(&cfg, hosts, relay, enroll, relay_error)).into_response()
}

/// The `/v1/status` document, pure so its shape is a test. `relay` is the
/// live route only; `relay_enroll` carries the waiting draft's digest for
/// a paired phone to sign, and is null once a route lives, when nothing
/// is waiting, or when the road is off; `relay_error` says why nothing is
/// waiting when the relay server could not be reached; `road_order` is
/// the desktop's preference for phones that have not set their own.
fn status_body(
    cfg: &Config,
    hosts: Vec<String>,
    relay: Option<(String, u16)>,
    enroll_digest: Option<[u8; 32]>,
    relay_error: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "api": API_VERSION,
        "name": cfg.name,
        "version": env!("CARGO_PKG_VERSION"),
        "hosts": hosts,
        "iroh": if cfg.iroh_enabled { crate::iroh_tunnel::node_id_of(&cfg.iroh_secret) } else { None },
        "relay": relay.map(|(host, port)| serde_json::json!({ "host": host, "port": port })),
        "relay_enroll": enroll_digest.map(|d| serde_json::json!({ "digest": remote_roads::b64url(&d) })),
        "relay_error": relay_error,
        "roads": Roads::of(cfg),
        "road_order": cfg.road_order,
    })
}

#[derive(Deserialize)]
struct EnrollBody {
    /// base64url nopad, 33-byte compressed SEC1 P-256 point.
    authority_public_key: String,
    /// base64url nopad, DER ECDSA over the draft digest.
    signature_der: String,
}

/// The phone answers the waiting draft — the QR's `ta`, or the same digest
/// read from `/v1/status.relay_enroll` after pairing: it signed it with its
/// authority key, and the desktop — after checking the signature itself —
/// registers the route with the relay, keeps it, and starts the connector.
/// 409 when nothing is pending, 400 when the answer does not verify (also
/// what a phone gets for a digest a re-prepare has since replaced — it
/// re-reads status and signs the new one), 502 when the relay refused.
async fn relay_enroll(State(ctx): State<Ctx>, Json(b): Json<EnrollBody>) -> Response {
    let state = ctx.app.state::<RemoteState>();
    let draft = {
        let cfg = state.config.lock().unwrap();
        let relay = state.phone_relay.lock().unwrap();
        if !cfg.relay_enabled || relay.config.is_some() {
            None
        } else {
            relay.draft.as_ref().map(|(d, _)| d.clone())
        }
    };
    let Some(draft) = draft else {
        return err(StatusCode::CONFLICT, "no relay enrollment is pending");
    };
    let (Some(key), Some(sig)) = (
        remote_roads::b64url_decode(&b.authority_public_key),
        remote_roads::b64url_decode(&b.signature_der),
    ) else {
        return err(StatusCode::BAD_REQUEST, "authority_public_key and signature_der must be base64url");
    };
    if let Err(e) = remote_roads::verify_enrollment(draft.authorization_digest(), &key, &sig) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let config = match draft.register(&key, &sig).await {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e),
    };
    let root = match crate::remote::state_root() {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if let Err(e) = config.save_named(&root, PHONE_RELAY_FILE) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let port = state.config.lock().unwrap().port;
    {
        let mut relay = state.phone_relay.lock().unwrap();
        relay.draft = None;
        relay.failed = None;
        relay.config = Some(config.clone());
        if let Some(old) = relay.handle.take() {
            tokio::spawn(old.stop());
        }
    }
    start_phone_relay(&state, port);
    crate::diag!("remote", "phone relay route enrolled: {}:{}", config.public_host, config.public_port);
    notify(&ctx.app, Event::StatusChanged);
    let _ = ctx.app.emit("remote://clients", ());
    Json(serde_json::json!({ "host": config.public_host, "port": config.public_port })).into_response()
}

async fn agents(State(ctx): State<Ctx>) -> Response {
    let mut list = serde_json::to_value(crate::agents::agent_choices_from(
        ctx.app.state::<crate::services::ApplicationServices>().inner(),
    ))
    .unwrap_or_default();
    // Every keyed provider joins as a choice of its own — "OpenRouter"
    // beside the CLIs — carrying its FULL catalog: starred models first,
    // then everything the provider publishes. The id wears an api: prefix
    // so bring-in and new-session know the kind.
    if let Some(arr) = list.as_array_mut() {
        for p in crate::providers::load_providers() {
            if p.api_key.is_empty() {
                continue;
            }
            let catalog = cached_models(&ctx, &p.id).await;
            let mut models = p.startup_models.clone();
            for m in catalog {
                if !models.contains(&m) {
                    models.push(m);
                }
            }
            if models.is_empty() {
                continue;
            }
            arr.push(serde_json::json!({
                "id": format!("api:{}", p.id),
                "display_name": p.name,
                "models": models.iter().map(|m| serde_json::json!({
                    "id": m,
                    "display_name": m.split('/').next_back().unwrap_or(m),
                    "efforts": [],
                })).collect::<Vec<_>>(),
                "mints_session_id": false,
            }));
        }
    }
    Json(list).into_response()
}

/// A provider's /models ids, at most one real fetch per ten minutes.
async fn cached_models(ctx: &Ctx, provider_id: &str) -> Vec<String> {
    {
        let state = ctx.app.state::<RemoteState>();
        let cache = state.models_cache.lock().unwrap();
        if let Some((at, v)) = cache.get(provider_id) {
            if at.elapsed() < Duration::from_secs(600) {
                return v.clone();
            }
        }
    }
    let id = provider_id.to_string();
    let fetched = crate::run_blocking(move || crate::providers::provider_models(id).unwrap_or_default()).await;
    let state = ctx.app.state::<RemoteState>();
    state.models_cache.lock().unwrap().insert(provider_id.to_string(), (Instant::now(), fetched.clone()));
    fetched
}

/// The sidebar's list, plus what the phone renders as state: `running` (a
/// process holds this session), `open` (a desktop tab is bound to it —
/// input will land), and `activity` for open ones — "working" while the
/// agent reports progress, "attention" when it rang for a person, else
/// "idle". The desktop's terminal is the source of all three.
async fn sessions(State(ctx): State<Ctx>) -> Response {
    let t0 = Instant::now();
    let sessions = crate::sessions::list_sessions().await;
    let t_list = t0.elapsed();
    let running = crate::sessions::running_session_ids().await;
    let t_running = t0.elapsed();
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    let open = tabs.bound_sessions();
    let cadence: HashMap<String, String> = tabs.session_activities().into_iter().collect();
    // A terminal that reports nothing (Codex has no progress sequence) is
    // not idle — ask its transcript. Only for sessions with a live process.
    let candidates: Vec<String> = open.iter().chain(running.iter()).cloned().collect();
    let busy: HashMap<String, (&'static str, &'static str)> = crate::run_blocking(move || {
        candidates
            .into_iter()
            .filter_map(|id| transcript_verdict(&id).map(|v| (id, v)))
            .collect()
    })
    .await;
    // One verdict per session either source knows about, decided by the
    // same function the spine's phase tick calls — the two cannot disagree
    // about a session because there is only one rule.
    let spine = ctx.app.try_state::<std::sync::Arc<crate::spine::Spine>>();
    let activity: HashMap<String, String> = cadence
        .keys()
        .chain(busy.keys())
        .map(|id| {
            // The same turn state the spine's own phase tick reads, for
            // sessions it is tailing; `None` for the rest, which leaves
            // them on the cadence rule they had before.
            let turn = spine.as_ref().and_then(|s| s.turn_open(id));
            // And the same hook state: a session holding a permission
            // dialog reads needs-you in the list as well as in the session.
            let hooked = spine.as_ref().is_some_and(|s| s.hook_attention(id));
            let v =
                activity_verdict(cadence.get(id).map(String::as_str), busy.get(id).copied(), turn, hooked);
            (id.clone(), v.0.to_string())
        })
        .collect();
    let t_busy = t0.elapsed();
    // Which sessions produced files, for the phone's "has files" filter.
    let with_files: Vec<String> = crate::changes::sessions_with_files(&ctx.app).into_iter().collect();
    let t_files = t0.elapsed();
    // Dev servers: listening ports owned by each open session's process
    // tree, so the phone can offer a live preview of what's being built.
    let port_roots: Vec<(String, u32)> = open
        .iter()
        .filter_map(|id| tabs.child_pid_for_session(id).map(|pid| (id.clone(), pid)))
        .collect();
    let ports: HashMap<String, Vec<u16>> = crate::run_blocking(move || {
        port_roots
            .into_iter()
            .filter_map(|(id, pid)| {
                let p = ports_of_tree(pid);
                if p.is_empty() { None } else { Some((id, p)) }
            })
            .collect()
    })
    .await;
    crate::diag!(
        "remote",
        "sessions: list {}ms | running +{}ms | busy +{}ms | files +{}ms | ports +{}ms | total {}ms",
        t_list.as_millis(),
        (t_running - t_list).as_millis(),
        (t_busy - t_running).as_millis(),
        (t_files - t_busy).as_millis(),
        (t0.elapsed() - t_files).as_millis(),
        t0.elapsed().as_millis()
    );
    Json(serde_json::json!({
        "sessions": sessions,
        "running": running,
        "open": open,
        "activity": activity,
        "with_files": with_files,
        "ports": ports,
        "stars": crate::sessions::load_stars(),
        "brought_in": crate::sessions::load_brought_in(),
    }))
    .into_response()
}

/// Does the transcript say a turn is in progress? Codex writes
/// `task_started` and `task_complete` events; Claude's last message is the
/// person's until the assistant answers. Reads only the tail of the file.
fn transcript_busy(session_id: &str) -> bool {
    transcript_verdict(session_id).is_some()
}

/// The tail of `path`, at most `keep` bytes. `None` for a missing file or a
/// tail that is not valid UTF-8 from the seek point — the same shrug the
/// transcript read below gives.
fn tail_of(path: &std::path::Path, keep: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(keep))).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// The verdict from grok's explicit state events, read off the tail of the
/// session dir's `events.jsonl`. [observed: grok 1.0.13]
///
/// Grok now writes codex-style turn brackets plus something neither other
/// engine records — an explicit waiting-on-a-person event:
///
/// ```text
/// {"ts":"…","type":"turn_started","session_id":"…","turn_number":0,"model_id":"grok-4.6",…}
/// {"ts":"…","type":"permission_requested","tool_name":"write"}
/// {"ts":"…","type":"permission_resolved","tool_name":"write","decision":"allow","wait_ms":0}
/// {"ts":"…","type":"turn_ended","outcome":"completed"}
/// ```
///
/// These are transcript facts and outrank the open-tool_call + cadence
/// inference (HARNESS-CONTRACT.md, "The state machine"): an open bracket is
/// working, an unresolved `permission_requested` is attention with no
/// 45-second wait, and a closed bracket is idle even when `chat_history.jsonl`
/// ends on a bare user/tool_result line from a killed run — the case the
/// inference reads as stuck-working forever. A cancelled turn is still
/// `turn_ended` (`outcome:"cancelled"`), so the bracket closes either way.
///
/// Nested option: `Some(state)` is a verdict (`Some(None)` = idle); the
/// outer `None` means the tail carries no bracket at all — an events file
/// from before the first turn, or a tail cut inside one turn's phase spam —
/// and the caller falls back to the chat_history inference, which is also
/// all that older grok sessions (no events.jsonl) have.
fn grok_events_state(text: &str) -> Option<Option<&'static str>> {
    let (mut open_turn, mut open_permission, mut saw_bracket) = (false, false, false);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("turn_started") => {
                saw_bracket = true;
                open_turn = true;
                open_permission = false;
            }
            Some("turn_ended") => {
                saw_bracket = true;
                open_turn = false;
                open_permission = false;
            }
            Some("permission_requested") => open_permission = true,
            Some("permission_resolved") => open_permission = false,
            _ => {}
        }
    }
    if open_permission {
        // A fact on its own: the prompt is up whether or not the tail still
        // holds the turn_started that preceded it.
        return Some(Some("attention"));
    }
    saw_bracket.then(|| open_turn.then_some("working"))
}

/// The verdict from an antigravity transcript tail
/// (`~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl`).
/// [observed: agy 1.1.24]
///
/// agy writes one record per step, and the step's `type` says where the
/// turn is: a `USER_INPUT` is a prompt the model has not answered; a
/// `PLANNER_RESPONSE` carrying `tool_calls` is a call whose result has not
/// landed — attention when one of them is `ask_question`,
/// `ask_permission` or `ask_custom_permission`, the tools agy lists for
/// putting a question to the person; a `GENERIC` step is that result, which
/// the model now has to act on; a `PLANNER_RESPONSE` with `content` and no
/// calls is the answer, and the turn is over. `SYSTEM_MESSAGE` (the
/// "server restart" notice every resume adds) changes nothing. No process
/// check, exactly as grok's events arm: a killed run mid-turn reads working
/// until its next resume, which is the inference's known limit. And on an
/// account with `toolPermission: always-proceed` (this one) the ask_* tools
/// never fire, so attention never does either.
///
/// Nested option as [`grok_events_state`]: outer `None` = no record in the
/// tail; `Some(None)` = idle; `Some(Some(_))` = working or attention.
fn antigravity_transcript_state(text: &str) -> Option<Option<&'static str>> {
    let mut verdict: Option<Option<&'static str>> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("USER_INPUT") | Some("GENERIC") => verdict = Some(Some("working")),
            Some("PLANNER_RESPONSE") => {
                let calls = v.get("tool_calls").and_then(|c| c.as_array()).filter(|c| !c.is_empty());
                verdict = Some(match calls {
                    Some(calls) => {
                        let asks = calls.iter().any(|c| {
                            matches!(
                                c.get("name").and_then(|n| n.as_str()),
                                Some("ask_question" | "ask_permission" | "ask_custom_permission")
                            )
                        });
                        Some(if asks { "attention" } else { "working" })
                    }
                    None => None,
                });
            }
            _ => {}
        }
    }
    verdict
}

/// Whether agy's transcript ends on a tool call whose result has not
/// landed: the last step is a `PLANNER_RESPONSE` carrying `tool_calls`,
/// with no `GENERIC` (the result) after it. That is the only shape a
/// confirmation dialog can be sitting behind.
fn antigravity_open_call(text: &str) -> bool {
    let mut open = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("PLANNER_RESPONSE") => {
                open = v
                    .get("tool_calls")
                    .and_then(|c| c.as_array())
                    .is_some_and(|c| !c.is_empty());
            }
            // The result landing, or a new prompt, closes it.
            Some("GENERIC") | Some("USER_INPUT") => open = false,
            _ => {}
        }
    }
    open
}

/// agy's own log. A symlink into `log/cli-<stamp>.log` re-pointed on each
/// run; `metadata` and `File::open` both follow it, so this always reads
/// the current run's file.
pub(crate) fn antigravity_log_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".gemini/antigravity-cli/cli.log"))
}

/// The time in a glog header, as ms since the epoch.
///
/// `I0902 21:38:28.616360` is September 2nd at 21:38:28.616 LOCAL time —
/// glog writes no year and no zone. The year is this one, minus one when
/// that would place the line in the future (a December log read in
/// January). The line may be prefixed by agy's
/// `ERROR: logging before google.Init: `, so the header is found rather
/// than assumed to be first.
fn glog_time_ms(line: &str) -> Option<u64> {
    let mut fields = line.split_whitespace();
    let stamp = loop {
        let field = fields.next()?;
        // `I` + MMDD: the severity letter and the date, glued.
        let (Some(sev), true) = (field.chars().next(), field.len() == 5) else { continue };
        if !matches!(sev, 'I' | 'W' | 'E' | 'F') || !field[1..].bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        break field;
    };
    let month: i32 = stamp[1..3].parse().ok()?;
    let day: i32 = stamp[3..5].parse().ok()?;
    let clock = fields.next()?;
    let mut parts = clock.split(':');
    let hour: i32 = parts.next()?.parse().ok()?;
    let minute: i32 = parts.next()?.parse().ok()?;
    // `unwrap_or` evaluates its argument, so the field is taken once and
    // then split — asking `parts` for it twice consumed the iterator.
    let seconds = parts.next()?;
    let (sec, frac) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let second: i32 = sec.parse().ok()?;
    // glog writes microseconds; take whatever precision is actually there.
    let millis: u64 = format!("{frac:0<3}")[..3].parse().ok()?;

    // `mktime` is what turns a local civil time into an instant: it knows
    // this machine's zone and its DST rule, which no amount of arithmetic
    // here would. `tm_isdst = -1` asks it to work out which side of a
    // transition the time falls on.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let year = unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm.tm_year
    };
    let at = |year: i32| -> Option<u64> {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = year;
            tm.tm_mon = month - 1;
            tm.tm_mday = day;
            tm.tm_hour = hour;
            tm.tm_min = minute;
            tm.tm_sec = second;
            tm.tm_isdst = -1;
            let t = libc::mktime(&mut tm);
            (t != -1).then(|| t as u64 * 1000 + millis)
        }
    };
    let this_year = at(year)?;
    // More than a day ahead means the log rolled over a new year under us.
    if this_year > (now + 86_400) * 1000 {
        return at(year - 1);
    }
    Some(this_year)
}

/// Is agy sitting on a tool confirmation right now?
///
/// A permission dialog is INVISIBLE to the transcript: agy writes the
/// `PLANNER_RESPONSE` carrying the call and then nothing at all — no
/// `ask_*` tool, no further step — while its TUI waits for a person. The
/// only record anywhere is one line in agy's own log. Observed live: a
/// `run_command` sat on its dialog for minutes while the spine read
/// "working", with `tool_confirmation_manager.go:197] Surfacing tool
/// confirmation: "RunCommand" at step 2` the sole evidence.
/// [observed: agy 1.1.24, 2026-09-02]
///
/// `since_ms` is the transcript's mtime: a confirmation line NEWER than
/// the last thing the transcript learned is one still unanswered, because
/// answering it writes the result step and moves the transcript past it.
///
/// The log carries no conversation id, so this cannot say WHICH session
/// was asked. One agy TUI at a time is the normal case and the signal is
/// right for it; with two open, both sessions with an open call would read
/// `attention` off one prompt. Accepted: a false "come and look" on a
/// second session costs a glance, and the alternative is missing every
/// real one.
fn antigravity_confirmation_after(since_ms: u64) -> bool {
    let Some(path) = antigravity_log_path() else { return false };
    // 64 KB is many minutes of agy's chatter; the line we want is at the
    // very end of the file when it matters at all.
    let Some(text) = tail_of(&path, 64 * 1024) else { return false };
    text.lines()
        .filter(|l| l.contains("Surfacing tool confirmation"))
        .filter_map(glog_time_ms)
        .any(|at| at > since_ms)
}

/// When the transcript's verdict replaces what the terminal reported.
/// Cadence may promote to working, but it must not HOLD working against a
/// transcript that says a person is being waited on: codex's TUI keeps
/// animating (a ticking elapsed counter) while its approval dialog is up,
/// so cadence never goes quiet and, left alone, a session mid-approval
/// reads "working" forever — a brought-in codex sat exactly there
/// [observed: codex-cli 0.150.1]. Idle from cadence yields to any
/// transcript verdict (the old rule); attention beats working (this one).
/// A cadence "working" is never demoted to idle from here — output is
/// output.
fn transcript_outranks(terminal: &str, transcript: &str) -> bool {
    terminal == "idle" || (transcript == "attention" && terminal == "working")
}

/// The single place one session's activity is decided: the tab's output
/// cadence, corrected by what the session's own files say and by what a
/// Claude Code hook said as it happened. The sessions list and the spine's
/// phase tick both come through here, so neither can
/// hold a verdict the other would not.
///
/// Returns the verdict and a short human detail — "" when the source has
/// nothing to add beyond the state itself.
pub(crate) fn activity_verdict(
    terminal: Option<&str>,
    transcript: Option<(&'static str, &'static str)>,
    turn_open: Option<bool>,
    hook_attention: bool,
) -> (&'static str, &'static str) {
    // `session_activities` spells cadence "output"; the phone's session
    // state, the spine's phases and `transcript_outranks` all speak
    // working/attention/idle. Normalising here is what lets the rule below
    // fire at all: against the raw "output" spelling `transcript_outranks`
    // matched neither arm, so a codex parked on an approval kept reading as
    // busy — the exact case that rule was written for.
    let mut cadence: &'static str = match terminal {
        Some("output" | "working") => "working",
        Some("attention") => "attention",
        _ => "idle",
    };
    // Cadence is bytes on a pty, and a TUI goes on repainting after the
    // answer is finished — a spinner clearing, a footer redrawn, the prompt
    // coming back. Held on its own it kept the phone's header on "working"
    // for the ten seconds `session_activities` counts as recent, well after
    // the turn had visibly ended [observed: Claude Code, 2026-09-02]. So
    // when the spine's adapter has told us the turn is closed, cadence may
    // no longer promote to working. It may still say attention, and a new
    // `turn_started` re-opens the gate within a poll of the user's line
    // being written. `None` — the legacy adapter reports no turns at all —
    // leaves the old rule exactly as it was.
    if cadence == "working" && turn_open == Some(false) {
        cadence = "idle";
    }
    let verdict = match transcript {
        Some((state, detail)) if transcript_outranks(cadence, state) => (state, detail),
        _ => (cadence, ""),
    };
    // A Claude Code hook said a permission dialog is up. That is the harness
    // announcing its own state as it happens — not a file read after the
    // fact, not bytes on a pty — so it is the one input here that is not an
    // inference, and nothing below it may demote it. Cadence in particular
    // would: claude's TUI redraws its own dialog, so the pty is busy for as
    // long as the person takes to answer. It stands until a later hook (the
    // tool running, the turn ending) or the transcript retires it. A
    // transcript that already says attention keeps its own reason, which is
    // more specific than this one; the caller replaces even that with the
    // hook's detail when it has one ("permission: Edit").
    if hook_attention && verdict.0 != "attention" {
        return ("attention", "permission");
    }
    verdict
}

/// `Some(("working", …))`, `Some(("attention", …))` — codex mid-approval, or
/// a grok permission prompt — or `None`.
/// Public within the crate: the pty layer consults it before believing a
/// quiet terminal means an idle session, and the spine's phase tick turns
/// it into a `phase` event.
/// Codex writes nothing while its approval prompt is up, so "waiting on a
/// person" is read as: a turn in progress whose last act is a tool call
/// with no output, and a transcript that has gone quiet. Grok ≥1.0.13 writes
/// explicit events instead ([`grok_events_state`]), which short-circuit that
/// inference for grok sessions only.
///
/// The second half is a short human reason, "" when there is none. It is
/// never inferred: each return below names only what the record it read
/// actually says.
pub(crate) fn transcript_verdict(session_id: &str) -> Option<(&'static str, &'static str)> {
    // OpenCode sessions live in a SQLite store, not a transcript file —
    // `owner_in` resolves one to `opencode.db` itself, and the tail read
    // below then fails UTF-8 on binary SQLite into a silent `None`, every
    // call. Answer from the store instead: the newest assistant message row
    // with `time.completed` still NULL is a turn in flight; completed means
    // no busy claim. A killed run leaves the NULL forever, so "working" also
    // requires a live process holding the session (argv naming the id, or an
    // `opencode` in the session's directory for a fresh launch whose argv
    // names no session yet). No needs-you verdict exists to give: OpenCode's
    // permission config auto-answers, and its TUI emits no OSC 9;4 and no
    // bell — output cadence and this bracket are the only signals.
    // [observed: opencode 1.18.25]
    if crate::opencode::valid_id(session_id) {
        return match crate::opencode::open_turn(session_id) {
            Some((true, dir))
                if crate::sessions::opencode_process_alive(session_id, dir.as_deref()) =>
            {
                Some(("working", ""))
            }
            _ => None,
        };
    }
    let list = crate::agents::backends();
    let Some((_, path)) = crate::agents::owner_in(&list, session_id) else { return None };
    // Grok ≥1.0.13: the transcript sits in a session DIRECTORY named by the
    // session id, and `events.jsonl` beside it carries explicit state events
    // that replace the inference below. Grok only by construction: claude and
    // codex transcripts never sit in a directory named after their session,
    // so they cannot take this branch. Older grok sessions have no
    // events.jsonl and fall through to the open-tool_call inference.
    // [observed: grok 1.0.13]
    if let Some(dir) = path.parent().filter(|d| d.file_name().is_some_and(|n| n == session_id)) {
        if let Some(text) = tail_of(&dir.join("events.jsonl"), 256 * 1024) {
            if let Some(verdict) = grok_events_state(&text) {
                // That function returns attention for exactly one reason —
                // an unresolved `permission_requested`; nothing else in
                // events.jsonl can produce it — so naming the reason here
                // reads the record rather than guessing at it.
                return verdict.map(|s| (s, if s == "attention" { "permission" } else { "" }));
            }
        }
    }
    // Antigravity: the transcript sits under `…/antigravity-cli/brain/<id>/`
    // and its step types say where the turn is — the generic parser below
    // knows none of them, so the verdict comes from the tail alone.
    // [observed: agy 1.1.24]
    if path.to_string_lossy().contains("/antigravity-cli/brain/") {
        let text = tail_of(&path, 256 * 1024)?;
        let verdict = antigravity_transcript_state(&text).flatten();
        // An open call plus a confirmation line newer than the transcript
        // is a dialog still on screen — the one state agy's own records
        // cannot express. See `antigravity_confirmation_after`.
        if verdict == Some("working") && antigravity_open_call(&text) {
            let written = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if antigravity_confirmation_after(written) {
                return Some(("attention", "permission"));
            }
        }
        // agy's other attention is an unanswered `ask_question` /
        // `ask_permission` / `ask_custom_permission` call.
        return verdict.map(|s| (s, if s == "attention" { "permission" } else { "" }));
    }
    let stale = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e > Duration::from_secs(45));
    let Ok(mut f) = std::fs::File::open(&path) else { return None };
    use std::io::{Read, Seek, SeekFrom};
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(128 * 1024);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return None;
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return None;
    }
    let mut state: Option<bool> = None;
    let mut pending_call = false;
    // Codex-shaped records seen: gates the no-pending-call attention
    // fallback below to codex rollouts only.
    let mut saw_codex = false;
    for line in buf.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
            continue;
        }
        match v.get("type").and_then(|t| t.as_str()) {
            Some("event_msg") => { saw_codex = true; match v.pointer("/payload/type").and_then(|t| t.as_str()) {
                Some("task_started") => { state = Some(true); pending_call = false }
                Some("task_complete") | Some("turn_aborted") => { state = Some(false); pending_call = false }
                _ => {}
            } },
            Some("turn_context") | Some("session_meta") => saw_codex = true,
            Some("response_item") => { saw_codex = true; match v.pointer("/payload/type").and_then(|t| t.as_str()) {
                Some("custom_tool_call") | Some("function_call") => pending_call = true,
                Some("custom_tool_call_output") | Some("function_call_output") => pending_call = false,
                _ => {}
            } },
            Some("user") => {
                // A tool result is Claude talking to itself, not a new ask.
                let is_result = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|a| a.iter().all(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")));
                if !is_result {
                    state = Some(true);
                } else {
                    state = Some(true); // mid-turn: the model has a result to act on
                }
            }
            Some("assistant") => {
                // Text without a tool call ends the turn; a tool call means
                // more to come. Claude nests tool_use in /message/content;
                // grok puts tool_calls at the top of the line.
                let claude_tool = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|a| a.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")));
                let grok_tool = v
                    .get("tool_calls")
                    .and_then(|c| c.as_array())
                    .is_some_and(|a| !a.is_empty());
                state = Some(claude_tool || grok_tool);
            }
            // Grok writes tool results as their own lines: the model has a
            // result to act on, so the turn is still going.
            Some("tool_result") => state = Some(true),
            _ => {}
        }
    }
    match state {
        Some(true) if pending_call && stale => Some(("attention", "a tool call is waiting")),
        // Codex asks for command approval BEFORE writing the exec record, so
        // a dialog can be up with NO unanswered call on disk — a live stuck
        // approval showed exactly that: open turn, all steps completed, phone
        // said "working" [observed: codex-cli 0.150.1, 2026-08-31; the audit
        // found no approval record type in any rollout 0.144→0.150.1]. For
        // codex files only: an open turn that has written nothing for 45s is
        // a person being waited on — or a wedge, which wants the same glance.
        // Claude keeps the pending-call requirement: its long silent Bash
        // calls are routine, and its prompts ring the terminal bell instead.
        Some(true) if saw_codex && stale => Some(("attention", "approval")),
        Some(true) => Some(("working", "")),
        _ => None,
    }
}

/// The same numbers the desktop's usage strip shows: plan limits per engine
/// and provider balances. Slow — it asks each service — so the phone asks
/// rarely and shows the last answer.
async fn usage(State(ctx): State<Ctx>) -> Response {
    let fresh = crate::run_blocking(crate::usage::usage_report).await;
    let state = ctx.app.state::<RemoteState>();
    let mut cache = state.usage_cache.lock().unwrap();
    let report: Vec<crate::usage::UsageSource> = fresh
        .into_iter()
        .map(|u| {
            if u.state == "ok" {
                cache.insert(u.id.clone(), u.clone());
                u
            } else {
                cache.get(&u.id).cloned().unwrap_or(u)
            }
        })
        .collect();
    Json(report).into_response()
}

const UPLOAD_LIMIT: usize = 25 * 1024 * 1024;

/// A file from the phone — a screenshot, a document — lands on the desktop's
/// disk, and the phone puts its path in the message. That is how a CLI agent
/// takes an attachment: by reading it where it sits.
async fn upload(headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let raw = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("upload");
    let name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .trim_matches('.')
        .chars()
        .take(80)
        .collect();
    let name = if name.is_empty() { "upload".to_string() } else { name };
    if body.len() > UPLOAD_LIMIT {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "25 MB at most");
    }
    let Some(dir) = dirs::data_dir().map(|d| d.join("aiterm").join("uploads")) else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "no data dir");
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{stamp}-{name}"));
    if let Err(e) = std::fs::write(&path, &body) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(serde_json::json!({ "path": path.to_string_lossy(), "bytes": body.len() })).into_response()
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

/// The desktop's own full-text index over transcripts — the same answer
/// the sidebar's search box gives.
async fn search(Query(q): Query<SearchQuery>) -> Response {
    if q.q.trim().is_empty() {
        return Json(Vec::<crate::sessions::Session>::new()).into_response();
    }
    Json(crate::indexer::search_sessions(q.q).await).into_response()
}

/// Files a session wrote, by tool and time — what the desktop's panel lists.
async fn artifacts(Path(id): Path<String>) -> Response {
    Json(crate::sessions::session_artifacts(id).await).into_response()
}

#[derive(Serialize)]
struct FileEntry {
    path: String,
    name: String,
    bytes: u64,
    /// Unix seconds, last modified.
    modified: u64,
    /// How we know about it: "wrote" (a tool call in the transcript) or
    /// "changed" (newer than the session in its folder).
    via: String,
}

const FILES_CAP: usize = 300;
const FILES_DEPTH: usize = 9;
const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".cache", ".gradle", ".venv", "venv", "__pycache__", "dist", "build", ".next", ".kotlin"];

/// Everything a session produced, as best the desktop can tell: files its
/// transcript says it wrote, plus every file in its folder newer than the
/// session itself. The second list is what makes this work for any
/// harness — Codex, a shell script, an image model — not only the ones
/// whose tool calls we parse. Newest first.
async fn session_files(State(ctx): State<Ctx>, Path(id): Path<String>) -> Response {
    let wrote = crate::sessions::session_artifacts(id.clone()).await;
    let detail = crate::detail::session_detail(id.clone()).await;
    let ledger = crate::changes::for_session(&ctx.app, &id);
    let sid = id;
    let out = crate::run_blocking(move || {
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<FileEntry> = Vec::new();
        for a in wrote {
            if let Some(e) = file_entry(std::path::Path::new(&a.path), "wrote") {
                if seen.insert(e.path.clone()) {
                    out.push(e);
                }
            }
        }
        // The ledger's word first: what the filesystem saw this session do.
        for c in ledger {
            if c.kind == "deleted" {
                continue;
            }
            if let Some(e) = file_entry(std::path::Path::new(&c.path), if c.kind == "created" { "made" } else { "edited" }) {
                if seen.insert(e.path.clone()) {
                    out.push(e);
                }
            }
        }
        // Where a harness puts what it makes outside the project: Codex's
        // image generation writes to its own directory, keyed by session.
        for dir in harness_output_dirs(&sid) {
            walk_recent(&dir, 0, 0, &mut |e| {
                if out.len() < FILES_CAP && seen.insert(e.path.clone()) {
                    out.push(FileEntry { via: "made".into(), ..e });
                }
            });
        }
        if let Some(d) = detail {
            // Files changed while the session was alive — from its start to
            // a little after its last word. A session that ended yesterday
            // does not get credit for what another one did today in the
            // same folder; a session running now keeps gaining files.
            let since = d.started.as_deref().and_then(parse_iso_secs).unwrap_or(0);
            let until = d.last_active.as_deref().and_then(parse_iso_secs).map(|t| t + 15 * 60).unwrap_or(u64::MAX);
            if let Some(cwd) = d.cwd.as_deref() {
                walk_recent(std::path::Path::new(cwd), since, 0, &mut |e| {
                    if e.modified <= until && out.len() < FILES_CAP && seen.insert(e.path.clone()) {
                        out.push(e);
                    }
                });
            }
        }
        // What the agent made or said it wrote comes first; the folder's
        // other changes follow. Newest first within each.
        let rank = |v: &str| match v { "made" => 0, "edited" => 1, "wrote" => 2, _ => 3 };
        out.sort_by(|a, b| rank(&a.via).cmp(&rank(&b.via)).then(b.modified.cmp(&a.modified)));
        out
    })
    .await;
    Json(out).into_response()
}

use crate::changes::harness_output_dirs;

fn file_entry(path: &std::path::Path, via: &str) -> Option<FileEntry> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let modified = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(FileEntry {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        bytes: md.len(),
        modified,
        via: via.into(),
    })
}

fn walk_recent(dir: &std::path::Path, since: u64, depth: usize, push: &mut dyn FnMut(FileEntry)) {
    if depth > FILES_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_recent(&path, since, depth + 1, push);
        } else if ft.is_file() {
            if let Some(e) = file_entry(&path, "changed") {
                if e.modified >= since {
                    push(e);
                }
            }
        }
    }
}

/// "2026-08-29T14:36:55.009Z" → seconds. Enough of ISO 8601 for a
/// transcript's own timestamps; anything else is "since forever".
fn parse_iso_secs(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = rest.trim_end_matches('Z');
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut t = time.split(':');
    let (h, mi) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let sec = t.next().and_then(|x| x.split('.').next()).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    // Days from civil (Howard Hinnant), no calendar crate needed.
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + h * 3600 + mi * 60 + sec;
    u64::try_from(secs).ok()
}

/// The ledger's word on a session: every file it created, modified or
/// deleted, as the filesystem saw it. Newest first.
async fn session_changes(State(ctx): State<Ctx>, Path(id): Path<String>) -> Response {
    Json(crate::changes::for_session(&ctx.app, &id)).into_response()
}

/// One directory of a workspace, for the phone's explorer. Same guard as
/// files: only inside a project folder that has sessions.
async fn browse(Query(q): Query<FileQuery>) -> Response {
    let Ok(real) = std::path::PathBuf::from(&q.path).canonicalize() else {
        return err(StatusCode::NOT_FOUND, "no such folder");
    };
    // Directory listings are allowed anywhere under home — picking where a
    // NEW session lives means walking the tree, not only where sessions
    // already are. Serving file *contents* stays gated the strict way.
    if !real.is_dir() || !(under_home(&real) || file_is_allowed(&real).await) {
        return err(StatusCode::FORBIDDEN, "not a browsable folder");
    }
    match crate::fsx::list_dir(real.to_string_lossy().into_owned()).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

fn under_home(p: &std::path::Path) -> bool {
    dirs::home_dir().map(|h| p.starts_with(h)).unwrap_or(false)
}

#[derive(Deserialize)]
struct DirBody {
    path: String,
}

// ------------------------------------------------------------- phone terminal
// A plain shell on the desktop, driven from the phone: the same blank
// terminal the desktop's home launcher opens, as a tab the desktop shows
// too. The tab is opened straight in the registry (no renderer round trip),
// read back as flattened screen text, and written to by tab id — a shell
// runs no session, so the session-keyed routes cannot address it.

#[derive(Deserialize)]
struct TerminalOpenBody {
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

async fn terminal_open(State(ctx): State<Ctx>, Json(b): Json<TerminalOpenBody>) -> Response {
    let cwd = match b.cwd {
        Some(c) => {
            let p = std::path::PathBuf::from(&c);
            if c.contains("..") || !under_home(&p) || !p.is_dir() {
                return err(StatusCode::FORBIDDEN, "only a folder under home");
            }
            c
        }
        None => match dirs::home_dir() {
            Some(h) => h.to_string_lossy().into_owned(),
            None => return err(StatusCode::INTERNAL_SERVER_ERROR, "no home directory"),
        },
    };
    let title = std::path::Path::new(&cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Terminal".into());
    let Ok(size) = crate::remote::model::TerminalSize::try_new(
        b.cols.unwrap_or(80).clamp(20, 500),
        b.rows.unwrap_or(24).clamp(5, 200),
    ) else {
        return err(StatusCode::BAD_REQUEST, "bad terminal size");
    };
    // A slot of its own per open: this is a new terminal, not a return to one.
    let slot = format!("shell:remote:{}", uuid::Uuid::new_v4());
    let launch = crate::tabs::TabLaunch::new(title.clone(), slot, size).with_cwd(cwd.clone());
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>().inner().clone();
    let opened = crate::run_blocking(move || tabs.open_desktop(launch)).await;
    match opened {
        Ok(id) => Json(serde_json::json!({
            "tab_id": id.as_str(),
            "title": title,
            "cwd": cwd,
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The screen as text: scrollback tail plus the visible rows, one string
/// per row, wide-cell continuations skipped. Colors and styles stay on the
/// desktop — the phone gets what the terminal says, not how it looks.
#[derive(Serialize)]
struct TabRow {
    tab: String,
    slot: String,
    title: String,
    agent: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    running: bool,
}

/// Every tab the desktop holds, so a phone can name one.
///
/// `/v1/terminal/{tab}/screen` has always existed, but a tab id is minted
/// on the desktop and appeared in no answer — so an engine stuck before it
/// has written anything (codex asking whether to trust a directory, with no
/// rollout yet for `/v1/sessions` to list) could be neither seen nor
/// answered from the phone. Read-only: opening, writing and closing tabs
/// already have their own routes.
async fn tabs(State(ctx): State<Ctx>) -> Response {
    let registry = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    let rows: Vec<TabRow> = registry
        .list()
        .into_iter()
        .map(|t| TabRow {
            tab: t.id().as_str().to_string(),
            slot: t.slot_id().to_string(),
            title: t.title().to_string(),
            agent: t.agent_id().map(str::to_owned),
            session_id: t.session_id().map(str::to_owned),
            cwd: t.cwd().map(str::to_owned),
            running: *t.state() == crate::tabs::TabState::Running,
        })
        .collect();
    Json(rows).into_response()
}

async fn terminal_screen(State(ctx): State<Ctx>, Path(tab): Path<String>) -> Response {
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    let snap = match tabs.snapshot(&crate::tabs::TabId::from_raw(tab)) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::NOT_FOUND, e.to_string()),
    };
    fn row_text(row: &crate::terminal::model::ScreenRow) -> String {
        let mut s: String = row
            .cells()
            .iter()
            .filter(|c| !c.is_continuation())
            .map(|c| c.text())
            .collect();
        while s.ends_with(' ') {
            s.pop();
        }
        s
    }
    const SCROLLBACK_TAIL: usize = 400;
    let back = snap.scrollback();
    let start = back.len().saturating_sub(SCROLLBACK_TAIL);
    let lines: Vec<String> = back[start..]
        .iter()
        .chain(snap.visible().iter())
        .map(row_text)
        .collect();
    Json(serde_json::json!({
        "lines": lines,
        "cols": snap.cols(),
        "rows": snap.rows(),
    }))
    .into_response()
}

async fn terminal_input(
    State(ctx): State<Ctx>,
    Path(tab): Path<String>,
    Json(body): Json<InputBody>,
) -> Response {
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    let id = crate::tabs::TabId::from_raw(tab);
    if let Err(e) = tabs.write_tab_str(&id, &body.text) {
        return err(StatusCode::CONFLICT, e);
    }
    if body.enter.unwrap_or(true) {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if let Err(e) = tabs.write_tab_str(&id, "\r") {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Close the terminal tab — the deliberate act of being done with it, so
/// the tab goes on the desktop too rather than lingering as an exit notice.
async fn terminal_close(State(ctx): State<Ctx>, Path(tab): Path<String>) -> Response {
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>().inner().clone();
    let id = crate::tabs::TabId::from_raw(tab);
    match crate::run_blocking(move || tabs.close(&id)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

/// Make a directory (and its parents) under home — the phone's "new
/// project folder". Textual check on top of the home guard: no `..`.
async fn make_dir(Json(b): Json<DirBody>) -> Response {
    let path = std::path::PathBuf::from(&b.path);
    if b.path.contains("..") || !under_home(&path) {
        return err(StatusCode::FORBIDDEN, "only under the home folder");
    }
    match std::fs::create_dir_all(&path) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// Read one file the agent produced, by path, with ranges (video seeks)
/// and a content type from the extension. Only files inside a project
/// folder that has sessions, or in uploads/, are served — the phone sees
/// what the agents make, not the disk.
async fn file(Query(q): Query<FileQuery>, req: Request) -> Response {
    use tower::ServiceExt;
    let path = std::path::PathBuf::from(&q.path);
    let Ok(real) = path.canonicalize() else {
        return err(StatusCode::NOT_FOUND, "no such file");
    };
    if !real.is_file() || !file_is_allowed(&real).await {
        return err(StatusCode::FORBIDDEN, "not a file an agent produced here");
    }
    match tower_http::services::ServeFile::new(&real).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn file_is_allowed(real: &std::path::Path) -> bool {
    let mut always: Vec<PathBuf> = Vec::new();
    if let Some(up) = dirs::data_dir().map(|d| d.join("aiterm").join("uploads")) {
        always.push(up);
    }
    if let Some(home) = dirs::home_dir() {
        always.push(home.join(".codex").join("generated_images"));
        always.push(home.join(".grok").join("sessions"));
    }
    // Claude's scratchpads: session-owned throwaway output, phone-viewable.
    if real.to_string_lossy().starts_with("/tmp/claude-") {
        return true;
    }
    for dir in always {
        if let Ok(dir) = dir.canonicalize() {
            if real.starts_with(&dir) {
                return true;
            }
        }
    }
    let roots: Vec<PathBuf> = crate::sessions::list_sessions()
        .await
        .into_iter()
        .flat_map(|s| [PathBuf::from(s.project_path), PathBuf::from(s.group_path)])
        .collect();
    roots.iter().any(|r| r.canonicalize().map(|r| real.starts_with(r)).unwrap_or(false))
}

async fn detail(Path(id): Path<String>) -> Response {
    match crate::detail::session_detail(id).await {
        Some(d) => Json(d).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such session"),
    }
}

#[derive(Deserialize)]
struct ConversationQuery {
    max_chars: Option<usize>,
}

#[derive(Serialize)]
struct Turn {
    role: String,
    text: String,
}

async fn conversation(Path(id): Path<String>, Query(q): Query<ConversationQuery>) -> Response {
    let turns = crate::detail::conversation_rich(id, q.max_chars.unwrap_or(60_000)).await;
    let turns: Vec<Turn> = turns.into_iter().map(|(role, text)| Turn { role, text }).collect();
    Json(turns).into_response()
}

#[derive(Deserialize)]
struct SpineQuery {
    after: Option<u64>,
}

/// The spine's replay: everything after `after` for one session. Asking
/// registers interest, which is what starts (or keeps) the adapter tail —
/// see `docs/architecture/spine.md`. `live` false means the session is served by the
/// legacy adapter and the events are re-derived, not read from the engine.
async fn spine(State(ctx): State<Ctx>, Path(id): Path<String>, Query(q): Query<SpineQuery>) -> Response {
    match crate::spine::read_after(&ctx.app, &id, q.after.unwrap_or(0)).await {
        Some((epoch, live, events)) => {
            Json(serde_json::json!({ "epoch": epoch, "live": live, "events": events })).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "no such session"),
    }
}

/// Open (resume) a session in a desktop tab. The renderer owns tabs, so this
/// is a request to it, answered by `sessions.open` growing on the next list.
async fn open(State(ctx): State<Ctx>, Path(id): Path<String>) -> Response {
    crate::diag::write("remote", &format!("open {}", &id[..id.len().min(8)]));
    let _ = ctx.app.emit("remote://open-session", serde_json::json!({ "sessionId": id }));
    StatusCode::ACCEPTED.into_response()
}

#[derive(Deserialize)]
struct InputBody {
    text: String,
    /// Press Enter after the text. Default true — a message, not a keystroke.
    enter: Option<bool>,
}

async fn input(State(ctx): State<Ctx>, Path(id): Path<String>, Json(body): Json<InputBody>) -> Response {
    crate::diag::write("remote", &format!("input {} ({} chars, enter={})", &id[..id.len().min(8)], body.text.len(), body.enter.unwrap_or(true)));
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    if !tabs.has_session(&id) {
        return err(StatusCode::CONFLICT, "session is not open in a tab — open it first");
    }
    if let Err(e) = tabs.write_session_str(&id, &body.text) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    if body.enter.unwrap_or(true) {
        // A TUI that just took a paste needs a beat before the Enter, or it
        // reads the two as one and the line sits unsent.
        tokio::time::sleep(Duration::from_millis(60)).await;
        if let Err(e) = tabs.write_session_str(&id, "\r") {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Escape: what stops an agent's current turn in every TUI here, without
/// ending the session. A stop is a different, heavier thing (below).
#[derive(Deserialize)]
struct RenameBody {
    title: String,
}

#[derive(Deserialize)]
struct StarBody {
    on: bool,
}

#[derive(Deserialize)]
struct BringInBody {
    agent_id: String,
    model: Option<String>,
    effort: Option<String>,
    focus: Option<String>,
    rounds: Option<u32>,
    auto: Option<bool>,
}

/// The renderer runs the relay; this is how its state reaches the phones.
#[tauri::command]
pub fn relay_report(
    app: tauri::AppHandle,
    session_id: String,
    b_session_id: Option<String>,
    b_name: String,
    phase: String,
    round: u32,
    rounds: u32,
    note: String,
) {
    crate::diag!("relay", "{phase} r{round}/{rounds} a={} b={:?} ({b_name}) {note}", &session_id[..8.min(session_id.len())], b_session_id.as_deref().map(|b| &b[..8.min(b.len())]));
    if let Some(b) = b_session_id.as_deref() {
        if let Err(e) = crate::sessions::record_brought_in(b, &session_id) {
            crate::diag!("relay", "brought-in lineage not recorded: {e}");
        }
    }
    notify(&app, Event::Relay { session_id, b_session_id, b_name, phase, round, rounds, note });
}

/// The phone asks for a second agent; the desktop's renderer runs the
/// relay (it owns the tabs the two agents talk through). Needs the session
/// open in a tab — the phone opens it first.
async fn bring_in(State(ctx): State<Ctx>, Path(id): Path<String>, Json(b): Json<BringInBody>) -> Response {
    crate::diag::write("remote", &format!("bring-in {} <- {} model={:?} rounds={:?} auto={:?}", &id[..id.len().min(8)], b.agent_id, b.model, b.rounds, b.auto));
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    if !tabs.has_session(&id) {
        return err(StatusCode::CONFLICT, "open the session on the desktop first");
    }
    // An api:<provider> id is a model off a provider's startup list, not a
    // CLI — the renderer builds the matching StartChoice.
    let (kind, agent_id, provider_id) = match b.agent_id.strip_prefix("api:") {
        Some(pid) => ("api", String::new(), Some(pid.to_string())),
        None => ("agent", b.agent_id.clone(), None),
    };
    if kind == "api" && b.model.is_none() {
        return err(StatusCode::BAD_REQUEST, "an API choice needs a model");
    }
    let _ = ctx.app.emit(
        "remote://bring-in",
        serde_json::json!({
            "session_id": id,
            "kind": kind,
            "agent_id": agent_id,
            "provider_id": provider_id,
            "model": b.model,
            "effort": b.effort,
            "focus": b.focus.unwrap_or_default(),
            "rounds": b.rounds.unwrap_or(2).clamp(1, 3),
            "auto": b.auto.unwrap_or(false),
        }),
    );
    StatusCode::NO_CONTENT.into_response()
}

/// Star or unstar from the phone; both UIs re-read.
async fn star(State(ctx): State<Ctx>, Path(id): Path<String>, Json(b): Json<StarBody>) -> Response {
    match crate::sessions::set_star(&id, b.on) {
        Ok(()) => {
            notify(&ctx.app, Event::SessionsChanged);
            let _ = ctx.app.emit("sessions://changed", ());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Rename a session from the phone. The same override store the desktop
/// writes; both UIs hear about it and re-read.
async fn rename(State(ctx): State<Ctx>, Path(id): Path<String>, Json(b): Json<RenameBody>) -> Response {
    match crate::sessions::rename_session(&id, &b.title) {
        Ok(()) => {
            notify(&ctx.app, Event::SessionsChanged);
            let _ = ctx.app.emit("sessions://changed", ());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn interrupt(State(ctx): State<Ctx>, Path(id): Path<String>) -> Response {
    crate::diag::write("remote", &format!("interrupt {}", &id[..id.len().min(8)]));
    let tabs = ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>();
    if !tabs.has_session(&id) {
        return err(StatusCode::CONFLICT, "session is not open in a tab");
    }
    match tabs.write_session_str(&id, "\x1b") {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn stop_session(State(ctx): State<Ctx>, Path(id): Path<String>) -> Response {
    // A session open in a terminal tab is stopped by ending that tab's
    // process — the roster only knows Claude's daemon sessions, so going
    // through it for a tab (any engine) stopped nothing at all.
    if ctx.app.state::<std::sync::Arc<crate::tabs::TabRegistry>>().has_session(&id) {
        let app = ctx.app.clone();
        let sid = id.clone();
        crate::run_blocking(move || {
            app.state::<std::sync::Arc<crate::tabs::TabRegistry>>().kill_session_tab(&sid)
        })
        .await;
        return StatusCode::NO_CONTENT.into_response();
    }
    // An OpenCode id can never be in Claude's roster, so falling through
    // would find no entry and return Ok — a stop that stopped nothing,
    // reported as success, while any live `opencode` process kept running.
    // Refuse instead, the same answer input gives. [observed: opencode 1.18.25]
    if crate::opencode::valid_id(&id) {
        return err(StatusCode::CONFLICT, "session is not open in a tab — open it first");
    }
    match crate::sessions::stop_session(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
struct NewSessionBody {
    agent_id: String,
    cwd: String,
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    /// A name for the tab, when the person gave one.
    title: Option<String>,
}

async fn new_session(State(ctx): State<Ctx>, Json(body): Json<NewSessionBody>) -> Response {
    let _ = ctx.app.emit(
        "remote://new-session",
        serde_json::json!({
            "agentId": body.agent_id, "cwd": body.cwd, "prompt": body.prompt,
            "model": body.model, "effort": body.effort, "title": body.title,
        }),
    );
    StatusCode::ACCEPTED.into_response()
}

async fn events(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let state = ctx.app.state::<RemoteState>();
    let rx = state.events.subscribe();
    let spine_rx = ctx.app.state::<std::sync::Arc<crate::spine::Spine>>().subscribe();
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_string();
    let info = ClientInfo {
        id: state.next_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        device: h("x-aiterm-device").chars().take(80).collect::<String>().trim().to_string(),
        os: h("x-aiterm-os").chars().take(40).collect(),
        app: h("x-aiterm-app").chars().take(20).collect(),
        address: peer.ip().to_string(),
        since: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    };
    let app = ctx.app.clone();
    ws.on_upgrade(move |socket| async move {
        let id = info.id;
        crate::diag!("remote", "phone connected: {} ({}) from {}", info.device, info.os, info.address);
        app.state::<RemoteState>().clients.lock().unwrap().insert(id, info);
        let _ = app.emit("remote://clients", ());
        stream_events(socket, rx, spine_rx).await;
        app.state::<RemoteState>().clients.lock().unwrap().remove(&id);
        let _ = app.emit("remote://clients", ());
        crate::diag!("remote", "phone disconnected");
    })
}

async fn stream_events(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<Event>,
    mut spine_rx: broadcast::Receiver<crate::spine::SpineEvent>,
) {
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.tick().await; // the first tick is immediate; skip it
    // Cleared if the spine's channel ever closes, so a closed receiver
    // cannot spin this loop. It lives as long as the app, so this is
    // belt-and-braces.
    let mut spine_open = true;
    loop {
        tokio::select! {
            ev = spine_rx.recv(), if spine_open => {
                let ev = match ev {
                    Ok(ev) => Event::Spine(ev),
                    // A phone that missed spine events heals itself: the seq
                    // gap sends it back to GET …/spine?after=lastSeq. Making
                    // this a SessionsChanged would have it re-read the whole
                    // world instead, for nothing.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => { spine_open = false; continue }
                };
                let Ok(text) = serde_json::to_string(&ev) else { continue };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            ev = rx.recv() => {
                let ev = match ev {
                    Ok(ev) => ev,
                    // Fell behind: the phone re-reads on the next event anyway.
                    Err(broadcast::error::RecvError::Lagged(_)) => Event::SessionsChanged,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let Ok(text) = serde_json::to_string(&ev) else { continue };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                let Ok(text) = serde_json::to_string(&Event::Ping) else { continue };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attention from the transcript must beat a cadence "working": codex's
    /// TUI animates through its approval dialog, so cadence alone holds
    /// "working" forever. Idle yields to anything; working is never demoted
    /// to idle from the transcript side.
    #[test]
    fn transcript_attention_outranks_cadence_working() {
        assert!(transcript_outranks("working", "attention"));
        assert!(transcript_outranks("idle", "attention"));
        assert!(transcript_outranks("idle", "working"));
        assert!(!transcript_outranks("working", "working"));
        assert!(!transcript_outranks("working", "idle"));
        assert!(!transcript_outranks("attention", "working"));
    }

    /// The one rule both the sessions list and the spine's phase tick obey.
    /// The cases that matter: cadence is spelled "output" by the tab
    /// registry and must still lose to a transcript that says a person is
    /// being waited on, and the reason rides along with the verdict.
    #[test]
    fn one_verdict_serves_the_sessions_list_and_the_spine() {
        // Nothing known at all.
        assert_eq!(activity_verdict(None, None, None, false), ("idle", ""));
        // Cadence alone, in the spelling `session_activities` actually uses.
        assert_eq!(activity_verdict(Some("output"), None, None, false), ("working", ""));
        assert_eq!(activity_verdict(Some("idle"), None, None, false), ("idle", ""));
        // A quiet terminal takes any transcript verdict, reason and all.
        assert_eq!(
            activity_verdict(Some("idle"), Some(("working", "")), None, false),
            ("working", "")
        );
        assert_eq!(
            activity_verdict(Some("idle"), Some(("attention", "permission")), None, false),
            ("attention", "permission")
        );
        // The regression this function exists for: a codex whose TUI keeps
        // animating through its approval dialog reads "output" forever, and
        // must still come out as attention.
        assert_eq!(
            activity_verdict(Some("output"), Some(("attention", "approval")), None, false),
            ("attention", "approval")
        );
        // But output is never demoted to idle by a transcript with nothing
        // to say — output is output.
        assert_eq!(activity_verdict(Some("output"), None, None, false), ("working", ""));
        assert_eq!(
            activity_verdict(Some("output"), Some(("working", "")), None, false),
            ("working", "")
        );
    }

    /// The spine's turn bracket is the authority over cadence. A closed
    /// turn means a TUI's repaints no longer read as work; an open one, and
    /// a session no adapter reports turns for, leave the rule as it was.
    /// A verbatim line from `~/.gemini/antigravity-cli/cli.log`, agy's
    /// prefix and all. glog writes no year and no zone, so the assertion
    /// is a round trip through this machine's own zone rather than a
    /// constant that would only hold in one place.
    #[test]
    fn a_glog_header_parses_back_to_the_local_time_it_names() {
        const LINE: &str = "ERROR: logging before google.Init: I0902 21:38:28.616360     492 tool_confirmation_manager.go:197] Surfacing tool confirmation: \"RunCommand\" at step 2";
        let ms = glog_time_ms(LINE).expect("a glog header is in there");
        assert_eq!(ms % 1000, 616, "microseconds land as milliseconds");
        let (mon, day, h, mi, s) = unsafe {
            let t = (ms / 1000) as libc::time_t;
            let mut tm: libc::tm = std::mem::zeroed();
            assert!(!libc::localtime_r(&t, &mut tm).is_null());
            (tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
        };
        assert_eq!((mon, day, h, mi, s), (9, 2, 21, 38, 28));

        // The header is found, not assumed to be the first field.
        assert_eq!(glog_time_ms("I0902 21:38:28.616360 492 x.go:1] hi"), Some(ms));
        // Other severities, and a shorter fraction.
        assert!(glog_time_ms("W1231 00:00:00.5 1 x.go:1] hi").is_some());
        // Nothing that looks like a header.
        assert_eq!(glog_time_ms("just a line of text"), None);
        assert_eq!(glog_time_ms(""), None);
        assert_eq!(glog_time_ms("I09022 21:38:28.616360 1 x.go:1] hi"), None);
    }

    /// The shape a confirmation dialog sits behind: a call issued with no
    /// result after it. Anything that lands closes it.
    #[test]
    fn an_agy_call_is_open_only_until_its_result_lands() {
        let call = r#"{"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command"}]}"#;
        let result = r#"{"type":"GENERIC","content":"ok"}"#;
        let answer = r#"{"type":"PLANNER_RESPONSE","content":"done"}"#;
        let prompt = r#"{"type":"USER_INPUT","content":"go"}"#;
        assert!(antigravity_open_call(&format!("{prompt}\n{call}\n")));
        assert!(!antigravity_open_call(&format!("{prompt}\n{call}\n{result}\n")));
        assert!(!antigravity_open_call(&format!("{prompt}\n{call}\n{answer}\n")));
        assert!(!antigravity_open_call(prompt));
        assert!(!antigravity_open_call(""));
        // A second call after a landed result re-opens it.
        assert!(antigravity_open_call(&format!("{call}\n{result}\n{call}\n")));
    }

    #[test]
    fn a_closed_turn_stops_cadence_from_claiming_work() {
        // Some(false): the answer is finished, whatever the pty is doing.
        assert_eq!(activity_verdict(Some("output"), None, Some(false), false), ("idle", ""));
        assert_eq!(activity_verdict(Some("working"), None, Some(false), false), ("idle", ""));
        // Some(true) and None both leave cadence alone.
        assert_eq!(activity_verdict(Some("output"), None, Some(true), false), ("working", ""));
        assert_eq!(activity_verdict(Some("output"), None, None, false), ("working", ""));
        // Attention still outranks everything, from either side. A closed
        // turn with a permission prompt still up is a person being waited
        // on, not an idle session.
        assert_eq!(
            activity_verdict(Some("output"), Some(("attention", "permission")), Some(false), false),
            ("attention", "permission")
        );
        assert_eq!(
            activity_verdict(Some("attention"), None, Some(false), false),
            ("attention", "")
        );
        // And the transcript can still hold a session working against a
        // turn bracket that closed — the gate is on cadence only.
        assert_eq!(
            activity_verdict(Some("output"), Some(("working", "")), Some(false), false),
            ("working", "")
        );
        // A closed turn does not invent work out of a quiet terminal.
        assert_eq!(activity_verdict(Some("idle"), None, Some(false), false), ("idle", ""));
    }

    /// A hook is the harness announcing its own state, so it outranks the
    /// two inputs that are inferences. The case it exists for: claude's TUI
    /// redraws its permission dialog, so cadence reads "output" for as long
    /// as the person takes to answer, and without this the tick a second
    /// later would demote a needs-you the hook had just raised.
    #[test]
    fn a_hook_that_says_a_person_is_being_waited_on_is_not_demoted() {
        assert_eq!(activity_verdict(Some("output"), None, None, true), ("attention", "permission"));
        assert_eq!(activity_verdict(Some("idle"), None, None, true), ("attention", "permission"));
        // Including against a transcript that has not caught up — the tool
        // that is being asked about has not run, so nothing was written.
        assert_eq!(
            activity_verdict(Some("output"), Some(("working", "")), Some(true), true),
            ("attention", "permission")
        );
        // A transcript already saying attention keeps its own reason: it is
        // the more specific of the two.
        assert_eq!(
            activity_verdict(Some("output"), Some(("attention", "approval")), None, true),
            ("attention", "approval")
        );
        // And cadence's own attention is left as it was.
        assert_eq!(activity_verdict(Some("attention"), None, None, true), ("attention", ""));
        // False changes nothing at all: every other case is the old rule.
        assert_eq!(activity_verdict(Some("output"), None, None, false), ("working", ""));
    }

    /// The phone reads `type` to route the frame and `kind` to render it,
    /// both at the top level of one flat object. `Event` is internally
    /// tagged and `SpineEvent` carries its own `kind` tag through a
    /// `#[serde(flatten)]`, so this is the one place the two taggings meet.
    #[test]
    fn a_spine_frame_carries_both_tags_at_the_top_level() {
        let ev = Event::Spine(crate::spine::SpineEvent {
            seq: 42,
            epoch: 1788390000123,
            session_id: "abc".into(),
            agent: "claude".into(),
            ts: 1788390012345,
            kind: crate::spine::Kind::AgentText {
                id: "m1:0".into(),
                text: "on it".into(),
                done: false,
            },
        });
        let json: serde_json::Value = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(json["type"], "spine");
        assert_eq!(json["kind"], "agent_text");
        assert_eq!(json["seq"], 42);
        assert_eq!(json["epoch"], 1788390000123u64);
        assert_eq!(json["session_id"], "abc");
        assert_eq!(json["agent"], "claude");
        assert_eq!(json["ts"], 1788390012345u64);
        assert_eq!(json["id"], "m1:0");
        assert_eq!(json["text"], "on it");
        assert_eq!(json["done"], false);
        // Nothing nested: the phone parses one flat object.
        assert!(json.as_object().unwrap().values().all(|v| !v.is_object()));
    }

    #[test]
    fn a_token_is_64_hex_chars_and_never_repeats() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_eq_compares_whole_strings() {
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(!constant_eq(b"abc", b"ab"));
    }

    #[test]
    fn pem_decodes_to_the_der_it_wraps() {
        let der = b"\x30\x03\x02\x01\x05";
        let pem = "-----BEGIN CERTIFICATE-----\nMAMCAQU=\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_to_der(pem.as_bytes()).unwrap(), der);
    }

    #[test]
    fn iso_timestamps_become_unix_seconds() {
        assert_eq!(parse_iso_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_secs("2026-08-29T14:36:55.009Z"), Some(1788014215));
    }

    fn cfg() -> Config {
        Config {
            name: "office".into(),
            iroh_enabled: false,
            relay_enabled: true,
            road_order: vec!["iroh".into(), "relay".into(), "lan".into(), "vpn".into()],
            ..Config::default()
        }
    }

    #[test]
    fn status_carries_a_waiting_draft_for_a_paired_phone_to_sign() {
        let digest = [9u8; 32];
        let body = status_body(&cfg(), vec!["192.168.2.176".into()], None, Some(digest), None);
        assert!(body["relay"].is_null());
        assert_eq!(body["relay_enroll"]["digest"], remote_roads::b64url(&digest));
        assert!(body["relay_error"].is_null());
        assert_eq!(body["road_order"], serde_json::json!(["iroh", "relay", "lan", "vpn"]));
        assert_eq!(body["roads"]["relay"], true);
        assert_eq!(body["hosts"], serde_json::json!(["192.168.2.176"]));
    }

    #[test]
    fn status_with_a_live_route_offers_nothing_to_sign() {
        let body = status_body(&cfg(), vec![], Some(("desktop-1.relay.example.com".into(), 443)), None, None);
        assert_eq!(body["relay"], serde_json::json!({ "host": "desktop-1.relay.example.com", "port": 443 }));
        assert!(body["relay_enroll"].is_null());
    }

    #[test]
    fn status_says_why_no_draft_is_waiting() {
        let body = status_body(&cfg(), vec![], None, None, Some("the relay server could not be reached".into()));
        assert!(body["relay"].is_null());
        assert!(body["relay_enroll"].is_null());
        assert_eq!(body["relay_error"], "the relay server could not be reached");
    }

    #[test]
    fn a_config_written_before_road_order_existed_loads_the_default_order() {
        let json = r#"{"enabled":true,"port":8877,"token":"ab","name":"office"}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.road_order, remote_roads::default_road_order());
        let json = r#"{"enabled":true,"port":8877,"token":"ab","name":"office","road_order":["iroh","bogus"]}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        // Raw from serde; `load_config` normalizes, as the roads test shows.
        assert_eq!(remote_roads::normalize_road_order(&cfg.road_order), vec!["iroh", "lan", "vpn", "relay"]);
    }

    #[test]
    fn the_name_survives_a_uri() {
        assert_eq!(percent_encode("john-laptop"), "john-laptop");
        assert_eq!(percent_encode("John's PC"), "John%27s%20PC");
    }

    // grok_events_state — event lines below are verbatim from real grok
    // 1.0.13 sessions (harness-audit/grok.md §4).

    #[test]
    fn an_open_grok_turn_bracket_is_working() {
        let text = concat!(
            r#"{"ts":"2026-08-31T13:51:54.907Z","type":"turn_started","session_id":"01a05817-4052-7641-aa1f-18c5c845f914","turn_number":0,"model_id":"grok-4.6","yolo_mode":true,"conversation_message_count":3,"session_relationship":"primary","schema_version":"1.0"}"#, "\n",
            r#"{"ts":"2026-08-31T13:51:57.388Z","type":"phase_changed","phase":"streaming_reasoning"}"#, "\n",
        );
        assert_eq!(grok_events_state(text), Some(Some("working")));
    }

    #[test]
    fn a_closed_grok_turn_bracket_is_an_idle_verdict_not_a_fallback() {
        // The case the chat_history inference gets wrong: a transcript ending
        // on a bare tool_result reads as working forever; the closed bracket
        // says idle, and says it as a verdict.
        let text = concat!(
            r#"{"ts":"2026-08-31T13:51:54.907Z","type":"turn_started","session_id":"01a05817-4052-7641-aa1f-18c5c845f914","turn_number":0,"model_id":"grok-4.6","yolo_mode":true,"conversation_message_count":3,"session_relationship":"primary","schema_version":"1.0"}"#, "\n",
            r#"{"ts":"2026-08-31T13:51:58.009Z","type":"turn_ended","outcome":"completed"}"#, "\n",
        );
        assert_eq!(grok_events_state(text), Some(None));
        // A cancelled turn closes the bracket the same way.
        let cancelled = concat!(
            r#"{"ts":"2026-08-17T11:42:20.000Z","type":"turn_started","session_id":"x","turn_number":4,"model_id":"grok-4.6","schema_version":"1.0"}"#, "\n",
            r#"{"ts":"2026-08-17T11:42:35.043Z","type":"turn_ended","outcome":"cancelled","cancellation_category":"mid_turn_abort","cancellation_context":{"trigger":"ctrl_c"}}"#, "\n",
        );
        assert_eq!(grok_events_state(cancelled), Some(None));
    }

    #[test]
    fn an_unresolved_grok_permission_is_attention() {
        let text = concat!(
            r#"{"ts":"2026-08-31T13:47:00.000Z","type":"turn_started","session_id":"e761294d-0000-0000-0000-000000000000","turn_number":1,"model_id":"grok-4.6","schema_version":"1.0"}"#, "\n",
            r#"{"ts":"2026-08-31T13:47:04.034Z","type":"permission_requested","tool_name":"write"}"#, "\n",
        );
        assert_eq!(grok_events_state(text), Some(Some("attention")));
        // Even when the tail was cut inside the turn and the bracket is gone,
        // the unresolved prompt alone is a fact.
        let cut = concat!(r#"{"ts":"2026-08-31T13:47:04.034Z","type":"permission_requested","tool_name":"write"}"#, "\n");
        assert_eq!(grok_events_state(cut), Some(Some("attention")));
    }

    #[test]
    fn a_resolved_grok_permission_goes_back_to_working() {
        let text = concat!(
            r#"{"ts":"2026-08-31T13:47:00.000Z","type":"turn_started","session_id":"e761294d-0000-0000-0000-000000000000","turn_number":1,"model_id":"grok-4.6","schema_version":"1.0"}"#, "\n",
            r#"{"ts":"2026-08-31T13:47:04.034Z","type":"permission_requested","tool_name":"write"}"#, "\n",
            r#"{"ts":"2026-08-31T13:47:04.034Z","type":"permission_resolved","tool_name":"write","decision":"allow","wait_ms":0}"#, "\n",
        );
        assert_eq!(grok_events_state(text), Some(Some("working")));
    }

    // antigravity_transcript_state — step records below are verbatim from
    // real agy 1.1.24 conversations on this machine (2026-09-02); the
    // ask_question call is the one shape not observed, because this account
    // runs toolPermission=always-proceed and nothing ever asks.

    const AGY_INPUT: &str = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-02T16:12:18Z","content":"<USER_REQUEST>\nReply with exactly: pong5\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-09-02T12:12:18-04:00.\n</ADDITIONAL_METADATA>"}"#;
    const AGY_CALL: &str = r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-02T15:56:15Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls -la /home/john/nanoclaw/projects/google_ads/\"","Cwd":"\"/home/john/nanoclaw\"","RequestedTerminalID":"\"\"","RunPersistent":"false","WaitMsBeforeAsync":"5000","toolAction":"\"Listing google ads dir\"","toolSummary":"\"List files in projects/google_ads\""}}]}"#;
    const AGY_RESULT: &str = r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-02T15:55:58Z","content":"Created At: 2026-09-02T11:55:58-04:00\nCompleted At: 2026-09-02T11:56:15-04:00\n\nThe command exited with code 0.\nOutput:\n","truncated_fields":["content"]}"#;
    const AGY_SYSTEM: &str = r#"{"step_index":3,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-09-02T16:06:37Z","content":"The following is a <SYSTEM_MESSAGE> not actually sent by the user.\n\n<SYSTEM_MESSAGE>\n[Message] timestamp=2026-09-02T16:06:37Z sender=system priority=MESSAGE_PRIORITY_LOW content=[Notice] All your subagents and background tasks have been stopped due to server restart.\n</SYSTEM_MESSAGE>"}"#;
    const AGY_ANSWER: &str = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-02T16:12:18Z","content":"pong5"}"#;

    #[test]
    fn an_unanswered_antigravity_prompt_is_working() {
        assert_eq!(antigravity_transcript_state(AGY_INPUT), Some(Some("working")));
        // The server-restart notice a resume adds does not answer anything.
        let resumed = [AGY_INPUT, AGY_SYSTEM].join("\n");
        assert_eq!(antigravity_transcript_state(&resumed), Some(Some("working")));
    }

    #[test]
    fn an_open_antigravity_tool_call_is_working_and_so_is_its_result() {
        let open = [AGY_INPUT, AGY_CALL].join("\n");
        assert_eq!(antigravity_transcript_state(&open), Some(Some("working")));
        // The result landed: the model has it to act on, the turn goes on.
        let landed = [AGY_INPUT, AGY_CALL, AGY_RESULT].join("\n");
        assert_eq!(antigravity_transcript_state(&landed), Some(Some("working")));
    }

    #[test]
    fn an_antigravity_answer_is_an_idle_verdict_not_a_fallback() {
        let done = [AGY_INPUT, AGY_ANSWER].join("\n");
        assert_eq!(antigravity_transcript_state(&done), Some(None));
        // A tail cut to the answer alone still says idle.
        assert_eq!(antigravity_transcript_state(AGY_ANSWER), Some(None));
        assert_eq!(antigravity_transcript_state(""), None);
        assert_eq!(antigravity_transcript_state("not json\n"), None);
    }

    #[test]
    fn an_unanswered_antigravity_question_is_attention() {
        let ask = r#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-02T16:20:00Z","tool_calls":[{"name":"ask_question","args":{"Question":"\"Overwrite the file?\"","toolSummary":"\"Ask before overwriting\""}}]}"#;
        let text = [AGY_INPUT, ask].join("\n");
        assert_eq!(antigravity_transcript_state(&text), Some(Some("attention")));
        // Answered: the result is a GENERIC step, and the turn is working again.
        let answered = [AGY_INPUT, ask, AGY_RESULT].join("\n");
        assert_eq!(antigravity_transcript_state(&answered), Some(Some("working")));
    }

    #[test]
    fn grok_events_without_a_bracket_are_no_verdict() {
        // Pre-first-turn bookkeeping only — the caller must fall back to the
        // chat_history inference rather than call the session idle.
        let text = concat!(
            r#"{"ts":"2026-08-31T13:51:54.002Z","type":"mcp_config_resolved","servers":[{"name":"headroom","transport":"stdio","source":"local"}],"disabled":[]}"#, "\n",
            r#"{"ts":"2026-08-31T13:51:54.485Z","type":"mcp_init_completed","total_servers":2,"succeeded":2,"failed":0,"auth_required":0,"total_tools":4,"duration_ms":471,"is_reinit":false}"#, "\n",
        );
        assert_eq!(grok_events_state(text), None);
        assert_eq!(grok_events_state(""), None);
    }
}

