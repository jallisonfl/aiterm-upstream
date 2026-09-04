use aiterm_relay_protocol::{enrollment_digest, DirectCookie, DirectId, DirectPacket, Frame};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path as FilePath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinSet;
use tokio::time::{timeout, Duration};

const CLIENT_HELLO_LIMIT: usize = 64 * 1024;
const CLIENT_HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTOR_QUEUE: usize = 256;
const STREAM_QUEUE: usize = 32;
const MAX_STREAMS_PER_CONNECTOR: usize = 128;
const MAX_PROVISION_ATTEMPTS_PER_MINUTE: u32 = 30;
const DIRECT_TTL: Duration = Duration::from_secs(30);
const MAX_DIRECT_RENDEZVOUS: usize = 4096;
const MAX_DIRECT_PER_ROUTE: usize = 8;

#[derive(Clone, Debug, Deserialize)]
pub struct RelayConfig {
    pub connector_listen: SocketAddr,
    pub ingress_listen: SocketAddr,
    #[serde(default)]
    pub direct_listen: Option<SocketAddr>,
    #[serde(default)]
    pub direct_public_host: Option<String>,
    #[serde(default)]
    pub direct_public_port: Option<u16>,
    pub public_domain: String,
    pub routes: Vec<RouteConfig>,
    #[serde(default)]
    pub provisioning: Option<ProvisioningConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RouteConfig {
    pub id: String,
    pub token_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProvisioningConfig {
    pub state_file: PathBuf,
    pub control_origin: String,
    pub connector_url: String,
    pub public_port: u16,
    pub max_routes: usize,
    pub max_routes_per_ip: usize,
    pub max_routes_per_authority: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionedRoute {
    id: String,
    token_sha256: String,
    authority_fingerprint: String,
    desktop_spki_sha256: String,
    source_ip: IpAddr,
    created_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisioningStore {
    routes: Vec<ProvisionedRoute>,
}

struct ProvisioningState {
    config: ProvisioningConfig,
    store: Mutex<ProvisioningStore>,
    attempts: Mutex<HashMap<IpAddr, ProvisionAttemptWindow>>,
}

struct ProvisionAttemptWindow {
    started: Instant,
    count: u32,
}

#[derive(Clone)]
struct RouteAuth {
    token_sha256: [u8; 32],
}

#[derive(Clone)]
struct Connector {
    generation: u64,
    outgoing: mpsc::Sender<Frame>,
    streams: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
}

#[derive(Clone)]
struct RelayState {
    public_domain: Arc<str>,
    direct_public_host: Arc<str>,
    direct_public_port: u16,
    routes: Arc<RwLock<HashMap<String, RouteAuth>>>,
    provisioning: Option<Arc<ProvisioningState>>,
    connectors: Arc<Mutex<HashMap<String, Connector>>>,
    next_generation: Arc<AtomicU64>,
    next_stream: Arc<AtomicU64>,
    direct: Arc<Mutex<HashMap<DirectId, DirectRendezvous>>>,
}

struct DirectRendezvous {
    route: String,
    desktop_cookie: DirectCookie,
    phone_cookie: DirectCookie,
    desktop: Option<SocketAddr>,
    phone: Option<SocketAddr>,
    created: Instant,
}

impl RelayState {
    fn from_config(config: &RelayConfig) -> Result<Self, String> {
        let public_domain = normalize_dns_name(&config.public_domain)
            .ok_or_else(|| "public_domain is not a valid DNS name".to_string())?;
        let direct_public_host = match config.direct_public_host.as_deref() {
            Some(value) => normalize_dns_name(value)
                .ok_or_else(|| "direct_public_host is not a valid DNS name".to_string())?,
            None => public_domain.clone(),
        };
        let direct_public_port = config
            .direct_public_port
            .unwrap_or(config.ingress_listen.port());
        if direct_public_port == 0 {
            return Err("direct_public_port must be nonzero".into());
        }
        let mut routes = HashMap::new();
        for route in &config.routes {
            if !valid_route_id(&route.id) || routes.contains_key(&route.id) {
                return Err(format!("invalid or duplicate route id {}", route.id));
            }
            let token_sha256 = decode_hex_32(&route.token_sha256)
                .ok_or_else(|| format!("route {} has an invalid token hash", route.id))?;
            routes.insert(route.id.clone(), RouteAuth { token_sha256 });
        }
        let provisioning = if let Some(provisioning) = &config.provisioning {
            validate_provisioning(provisioning)?;
            let stored = load_provisioning_store(&provisioning.state_file)?;
            for route in &stored.routes {
                if !valid_route_id(&route.id) || routes.contains_key(&route.id) {
                    return Err(format!(
                        "invalid or duplicate provisioned route {}",
                        route.id
                    ));
                }
                let token_sha256 = decode_hex_32(&route.token_sha256).ok_or_else(|| {
                    format!("provisioned route {} has an invalid token hash", route.id)
                })?;
                for (label, value) in [
                    ("authority fingerprint", &route.authority_fingerprint),
                    ("desktop identity", &route.desktop_spki_sha256),
                ] {
                    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(value.as_bytes())
                        .map_err(|_| {
                            format!("provisioned route {} has an invalid {label}", route.id)
                        })?;
                    if decoded.len() != 32 {
                        return Err(format!(
                            "provisioned route {} has an invalid {label}",
                            route.id
                        ));
                    }
                }
                routes.insert(route.id.clone(), RouteAuth { token_sha256 });
            }
            if routes.len() > provisioning.max_routes {
                return Err("configured routes exceed the provisioning limit".into());
            }
            Some(Arc::new(ProvisioningState {
                config: provisioning.clone(),
                store: Mutex::new(stored),
                attempts: Mutex::new(HashMap::new()),
            }))
        } else {
            None
        };
        Ok(Self {
            public_domain: public_domain.into(),
            direct_public_host: direct_public_host.into(),
            direct_public_port,
            routes: Arc::new(RwLock::new(routes)),
            provisioning,
            connectors: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(1)),
            next_stream: Arc::new(AtomicU64::new(1)),
            direct: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

pub async fn run(config: RelayConfig) -> Result<(), String> {
    let state = RelayState::from_config(&config)?;
    let connector_listener = TcpListener::bind(config.connector_listen)
        .await
        .map_err(|error| format!("connector listener: {error}"))?;
    let ingress_listener = TcpListener::bind(config.ingress_listen)
        .await
        .map_err(|error| format!("phone ingress listener: {error}"))?;
    let direct_listener = UdpSocket::bind(config.direct_listen.unwrap_or(config.ingress_listen))
        .await
        .map_err(|error| format!("direct rendezvous listener: {error}"))?;
    run_with_listeners(state, connector_listener, ingress_listener, direct_listener).await
}

async fn run_with_listeners(
    state: RelayState,
    connector_listener: TcpListener,
    ingress_listener: TcpListener,
    direct_listener: UdpSocket,
) -> Result<(), String> {
    let app = Router::new()
        .route("/v1/connect/{route}", get(connector_upgrade))
        .route("/v1/info", get(relay_info))
        .route("/v1/provision", post(provision_route))
        .route("/v1/direct/{route}", post(prepare_direct))
        .route("/v1/routes/{route}", delete(deprovision_route))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state.clone());
    let connector_server = axum::serve(
        connector_listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    );
    let ingress_server = serve_ingress(ingress_listener, state.clone());
    let direct_server = serve_direct(direct_listener, state.clone());
    tokio::select! {
        result = connector_server => result.map_err(|error| format!("connector server: {error}")),
        result = ingress_server => result,
        result = direct_server => result,
    }
}

#[derive(Serialize)]
struct DirectPrepareView {
    id: String,
    desktop_cookie: String,
    phone_cookie: String,
    host: String,
    port: u16,
    expires_in_millis: u64,
}

async fn prepare_direct(
    State(state): State<RelayState>,
    Path(route): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(expected) = state.routes.read().await.get(&route).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(token) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if !bool::from(actual.ct_eq(&expected.token_sha256)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut direct = state.direct.lock().await;
    reap_direct(&mut direct);
    if direct.len() >= MAX_DIRECT_RENDEZVOUS
        || direct.values().filter(|value| value.route == route).count() >= MAX_DIRECT_PER_ROUTE
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "direct connection limit reached",
        )
            .into_response();
    }
    let mut id = [0u8; aiterm_relay_protocol::DIRECT_ID_BYTES];
    loop {
        OsRng.fill_bytes(&mut id);
        if !direct.contains_key(&id) {
            break;
        }
    }
    let mut desktop_cookie = [0u8; aiterm_relay_protocol::DIRECT_COOKIE_BYTES];
    let mut phone_cookie = [0u8; aiterm_relay_protocol::DIRECT_COOKIE_BYTES];
    OsRng.fill_bytes(&mut desktop_cookie);
    OsRng.fill_bytes(&mut phone_cookie);
    direct.insert(
        id,
        DirectRendezvous {
            route,
            desktop_cookie,
            phone_cookie,
            desktop: None,
            phone: None,
            created: Instant::now(),
        },
    );
    Json(DirectPrepareView {
        id: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id),
        desktop_cookie: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(desktop_cookie),
        phone_cookie: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(phone_cookie),
        host: state.direct_public_host.to_string(),
        port: state.direct_public_port,
        expires_in_millis: DIRECT_TTL.as_millis() as u64,
    })
    .into_response()
}

fn reap_direct(direct: &mut HashMap<DirectId, DirectRendezvous>) {
    direct.retain(|_, value| value.created.elapsed() < DIRECT_TTL);
}

async fn serve_direct(socket: UdpSocket, state: RelayState) -> Result<(), String> {
    let mut bytes = [0u8; aiterm_relay_protocol::MAX_DIRECT_PACKET_BYTES];
    loop {
        let (count, peer) = socket
            .recv_from(&mut bytes)
            .await
            .map_err(|error| format!("direct rendezvous receive: {error}"))?;
        let Ok(packet) = DirectPacket::decode(&bytes[..count]) else {
            continue;
        };
        let (id, cookie, desktop) = match packet {
            DirectPacket::BindDesktop { id, cookie } => (id, cookie, true),
            DirectPacket::BindPhone { id, cookie } => (id, cookie, false),
            _ => continue,
        };
        let peers = {
            let mut direct = state.direct.lock().await;
            reap_direct(&mut direct);
            let Some(rendezvous) = direct.get_mut(&id) else {
                continue;
            };
            let expected = if desktop {
                &rendezvous.desktop_cookie
            } else {
                &rendezvous.phone_cookie
            };
            if !bool::from(cookie.ct_eq(expected)) {
                continue;
            }
            if desktop {
                rendezvous.desktop = Some(peer);
            } else {
                rendezvous.phone = Some(peer);
            }
            (rendezvous.desktop, rendezvous.phone)
        };
        let _ = socket
            .send_to(&DirectPacket::Bound { id }.encode(), peer)
            .await;
        if let (Some(desktop), Some(phone)) = peers {
            let _ = socket
                .send_to(&DirectPacket::Peer { id, address: phone }.encode(), desktop)
                .await;
            let _ = socket
                .send_to(
                    &DirectPacket::Peer {
                        id,
                        address: desktop,
                    }
                    .encode(),
                    phone,
                )
                .await;
        }
    }
}

async fn connector_upgrade(
    State(state): State<RelayState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let Some(expected) = state.routes.read().await.get(&route).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(token) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if !bool::from(actual.ct_eq(&expected.token_sha256)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade
        .max_message_size(aiterm_relay_protocol::MAX_FRAME_BYTES)
        .on_upgrade(move |socket| serve_connector(socket, route, state))
        .into_response()
}

#[derive(Serialize, Deserialize)]
struct ProvisionedRouteView {
    connector_url: String,
    public_host: String,
    public_port: u16,
    route_id: String,
}

#[derive(Serialize)]
struct RelayInfoView {
    control_origin: String,
    connector_url: String,
    public_domain: String,
    public_port: u16,
}

async fn relay_info(State(state): State<RelayState>) -> Response {
    let Some(provisioning) = &state.provisioning else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(RelayInfoView {
        control_origin: provisioning.config.control_origin.clone(),
        connector_url: provisioning.config.connector_url.clone(),
        public_domain: state.public_domain.to_string(),
        public_port: provisioning.config.public_port,
    })
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionRouteRequest {
    route_id: String,
    token_sha256: String,
    desktop_spki_sha256: String,
    authority_public_key: String,
    signature_der: String,
}

async fn provision_route(
    State(state): State<RelayState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ProvisionRouteRequest>,
) -> Response {
    let Some(provisioning) = &state.provisioning else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let source_ip = forwarded_ip(&headers, peer);
    if !allow_provision_attempt(provisioning, source_ip).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "relay setup request limit reached",
        )
            .into_response();
    }
    if !valid_route_id(&request.route_id) {
        return (StatusCode::BAD_REQUEST, "invalid route id").into_response();
    }
    let Some(token_sha256) = decode_hex_32(&request.token_sha256) else {
        return (StatusCode::BAD_REQUEST, "invalid connector credential hash").into_response();
    };
    let Ok(desktop_spki) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(request.desktop_spki_sha256.as_bytes())
    else {
        return (StatusCode::BAD_REQUEST, "invalid desktop identity").into_response();
    };
    let Ok(desktop_spki): Result<[u8; 32], _> = desktop_spki.try_into() else {
        return (StatusCode::BAD_REQUEST, "invalid desktop identity").into_response();
    };
    let Ok(authority_bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(request.authority_public_key.as_bytes())
    else {
        return (StatusCode::BAD_REQUEST, "invalid phone authority key").into_response();
    };
    let Ok(authority) = VerifyingKey::from_sec1_bytes(&authority_bytes) else {
        return (StatusCode::BAD_REQUEST, "invalid phone authority key").into_response();
    };
    let canonical_authority = authority.to_encoded_point(true);
    if canonical_authority.as_bytes() != authority_bytes.as_slice() {
        return (
            StatusCode::BAD_REQUEST,
            "phone authority key is not canonical",
        )
            .into_response();
    }
    let Ok(signature_bytes) =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(request.signature_der.as_bytes())
    else {
        return (StatusCode::BAD_REQUEST, "invalid phone authority signature").into_response();
    };
    let Ok(signature) = Signature::from_der(&signature_bytes) else {
        return (StatusCode::BAD_REQUEST, "invalid phone authority signature").into_response();
    };
    let digest = enrollment_digest(
        &provisioning.config.control_origin,
        &request.route_id,
        &token_sha256,
        &desktop_spki,
    );
    if authority.verify(&digest, &signature).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let authority_fingerprint = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(canonical_authority.as_bytes()));
    let mut stored = provisioning.store.lock().await;
    if stored
        .routes
        .iter()
        .any(|route| route.id == request.route_id)
        || state.routes.read().await.contains_key(&request.route_id)
    {
        return StatusCode::CONFLICT.into_response();
    }
    if stored
        .routes
        .iter()
        .filter(|route| route.source_ip == source_ip)
        .count()
        >= provisioning.config.max_routes_per_ip
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "route limit reached for this address",
        )
            .into_response();
    }
    if state.routes.read().await.len() >= provisioning.config.max_routes {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "relay route capacity reached",
        )
            .into_response();
    }
    if stored
        .routes
        .iter()
        .filter(|route| route.authority_fingerprint == authority_fingerprint)
        .count()
        >= provisioning.config.max_routes_per_authority
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "route limit reached for this phone authority",
        )
            .into_response();
    }
    let record = ProvisionedRoute {
        id: request.route_id.clone(),
        token_sha256: request.token_sha256.clone(),
        authority_fingerprint,
        desktop_spki_sha256: request.desktop_spki_sha256,
        source_ip,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let mut next = stored.clone();
    next.routes.push(record);
    if persist_provisioning_store(&provisioning.config.state_file, &next).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not persist relay route",
        )
            .into_response();
    }
    state
        .routes
        .write()
        .await
        .insert(request.route_id.clone(), RouteAuth { token_sha256 });
    *stored = next;
    Json(ProvisionedRouteView {
        connector_url: provisioning.config.connector_url.clone(),
        public_host: format!("{}.{}", request.route_id, state.public_domain),
        public_port: provisioning.config.public_port,
        route_id: request.route_id,
    })
    .into_response()
}

async fn allow_provision_attempt(provisioning: &ProvisioningState, source_ip: IpAddr) -> bool {
    let now = Instant::now();
    let mut attempts = provisioning.attempts.lock().await;
    attempts.retain(|_, window| now.duration_since(window.started) < Duration::from_secs(300));
    let window = attempts.entry(source_ip).or_insert(ProvisionAttemptWindow {
        started: now,
        count: 0,
    });
    if now.duration_since(window.started) >= Duration::from_secs(60) {
        window.started = now;
        window.count = 0;
    }
    if window.count >= MAX_PROVISION_ATTEMPTS_PER_MINUTE {
        return false;
    }
    window.count += 1;
    true
}

async fn deprovision_route(
    State(state): State<RelayState>,
    Path(route): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(provisioning) = &state.provisioning else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(token) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let mut stored = provisioning.store.lock().await;
    let Some(position) = stored.routes.iter().position(|candidate| {
        candidate.id == route
            && decode_hex_32(&candidate.token_sha256)
                .is_some_and(|expected| bool::from(actual.ct_eq(&expected)))
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut next = stored.clone();
    next.routes.remove(position);
    if persist_provisioning_store(&provisioning.config.state_file, &next).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not persist relay route",
        )
            .into_response();
    }
    state.routes.write().await.remove(&route);
    if let Some(connector) = state.connectors.lock().await.remove(&route) {
        close_all_streams(&connector.streams).await;
    }
    *stored = next;
    StatusCode::NO_CONTENT.into_response()
}

fn forwarded_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if peer.ip().is_loopback() {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.trim().parse().ok())
        {
            return forwarded;
        }
    }
    peer.ip()
}

fn validate_provisioning(config: &ProvisioningConfig) -> Result<(), String> {
    let control = url::Url::parse(&config.control_origin).ok();
    if !config.state_file.is_absolute()
        || control.as_ref().is_none_or(|url| {
            url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || !matches!(url.path(), "" | "/")
                || url.query().is_some()
                || url.fragment().is_some()
        })
        || !config.connector_url.starts_with("wss://")
        || !config.connector_url.ends_with("/v1/connect")
        || config.public_port == 0
        || config.max_routes == 0
        || config.max_routes_per_ip == 0
        || config.max_routes_per_ip > config.max_routes
        || config.max_routes_per_authority == 0
        || config.max_routes_per_authority > config.max_routes
    {
        return Err("relay provisioning configuration is invalid".into());
    }
    Ok(())
}

fn load_provisioning_store(path: &FilePath) -> Result<ProvisioningStore, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProvisioningStore::default())
        }
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn persist_provisioning_store(path: &FilePath, store: &ProvisioningStore) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "provisioned route path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(store).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| token.len() >= 32 && token.len() <= 256)
}

async fn serve_connector(socket: WebSocket, route: String, state: RelayState) {
    let generation = state.next_generation.fetch_add(1, Ordering::Relaxed);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(CONNECTOR_QUEUE);
    let streams = Arc::new(Mutex::new(HashMap::new()));
    let connector = Connector {
        generation,
        outgoing: outgoing_tx,
        streams: streams.clone(),
    };
    if let Some(old) = state
        .connectors
        .lock()
        .await
        .insert(route.clone(), connector)
    {
        close_all_streams(&old.streams).await;
    }

    let (mut sink, mut source) = socket.split();
    loop {
        tokio::select! {
            outbound = outgoing_rx.recv() => {
                let Some(frame) = outbound else { break };
                let Ok(encoded) = frame.encode() else { break };
                if sink.send(Message::Binary(encoded.into())).await.is_err() { break; }
            }
            inbound = source.next() => {
                let Some(Ok(message)) = inbound else { break };
                let Message::Binary(bytes) = message else {
                    if matches!(message, Message::Close(_)) { break; }
                    continue;
                };
                let Ok(frame) = Frame::decode(&bytes) else { break };
                match frame {
                    Frame::Data { stream_id, bytes } => {
                        let target = streams.lock().await.get(&stream_id).cloned();
                        let Some(target) = target else { continue };
                        if target.try_send(bytes).is_err() {
                            streams.lock().await.remove(&stream_id);
                            let encoded = Frame::Close {
                                stream_id,
                                reason: b"phone too slow".to_vec(),
                            }.encode().expect("fixed relay close is valid");
                            if sink.send(Message::Binary(encoded.into())).await.is_err() { break; }
                        }
                    }
                    Frame::Close { stream_id, .. } => {
                        streams.lock().await.remove(&stream_id);
                    }
                    Frame::Ping => {
                        if outgoing_rx.is_closed() { break; }
                        let encoded = Frame::Pong.encode().expect("fixed relay pong is valid");
                        if sink.send(Message::Binary(encoded.into())).await.is_err() { break; }
                    }
                    Frame::Pong => {}
                    Frame::Open { .. } => break,
                }
            }
        }
    }

    close_all_streams(&streams).await;
    let mut connectors = state.connectors.lock().await;
    if connectors
        .get(&route)
        .is_some_and(|current| current.generation == generation)
    {
        connectors.remove(&route);
    }
}

async fn close_all_streams(streams: &Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>) {
    streams.lock().await.clear();
}

async fn serve_ingress(listener: TcpListener, state: RelayState) -> Result<(), String> {
    let mut tasks = JoinSet::new();
    loop {
        let (socket, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let state = state.clone();
        tasks.spawn(async move { serve_phone(socket, state).await });
        while tasks.len() > 1024 {
            let _ = tasks.join_next().await;
        }
    }
}

async fn serve_phone(mut phone: TcpStream, state: RelayState) {
    let sni = match timeout(CLIENT_HELLO_TIMEOUT, peek_sni(&phone)).await {
        Ok(Ok(sni)) => sni,
        _ => return,
    };
    let Some(route) = route_from_sni(&sni, &state.public_domain) else {
        return;
    };
    let connector = state.connectors.lock().await.get(route).cloned();
    let Some(connector) = connector else { return };
    let stream_id = next_nonzero(&state.next_stream);
    let (toward_phone, mut from_connector) = mpsc::channel::<Vec<u8>>(STREAM_QUEUE);
    {
        let mut streams = connector.streams.lock().await;
        if streams.len() >= MAX_STREAMS_PER_CONNECTOR {
            return;
        }
        streams.insert(stream_id, toward_phone);
    }
    if connector
        .outgoing
        .send(Frame::Open { stream_id })
        .await
        .is_err()
    {
        connector.streams.lock().await.remove(&stream_id);
        return;
    }

    let (mut phone_read, mut phone_write) = phone.split();
    let mut buffer = vec![0u8; aiterm_relay_protocol::MAX_DATA_BYTES];
    loop {
        tokio::select! {
            read = phone_read.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let frame = Frame::Data { stream_id, bytes: buffer[..count].to_vec() };
                    if connector.outgoing.send(frame).await.is_err() { break; }
                }
            },
            inbound = from_connector.recv() => match inbound {
                Some(bytes) if phone_write.write_all(&bytes).await.is_ok() => {}
                _ => break,
            }
        }
    }
    connector.streams.lock().await.remove(&stream_id);
    let _ = connector
        .outgoing
        .send(Frame::Close {
            stream_id,
            reason: Vec::new(),
        })
        .await;
}

fn next_nonzero(counter: &AtomicU64) -> u64 {
    loop {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

fn route_from_sni<'a>(sni: &'a str, public_domain: &str) -> Option<&'a str> {
    let suffix = format!(".{public_domain}");
    let route = sni.strip_suffix(&suffix)?;
    valid_route_id(route).then_some(route)
}

fn valid_route_id(value: &str) -> bool {
    (8..=63).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn normalize_dns_name(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 253
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        None
    } else {
        Some(value)
    }
}

async fn peek_sni(socket: &TcpStream) -> Result<String, &'static str> {
    let mut bytes = vec![0u8; CLIENT_HELLO_LIMIT];
    let mut previous_count = 0;
    loop {
        socket.readable().await.map_err(|_| "socket unreadable")?;
        let count = socket
            .peek(&mut bytes)
            .await
            .map_err(|_| "client hello unreadable")?;
        if count == 0 {
            return Err("client closed before hello");
        }
        match parse_client_hello_sni(&bytes[..count]) {
            Ok(name) => return Ok(name),
            Err(ParseHelloError::Incomplete) if count < CLIENT_HELLO_LIMIT => {
                // Peeking leaves the existing prefix readable, so readiness
                // alone cannot tell us that another fragment arrived.
                if count == previous_count {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                previous_count = count;
                continue;
            }
            Err(ParseHelloError::Incomplete) => return Err("client hello is too large"),
            Err(ParseHelloError::Invalid) => return Err("invalid client hello"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ParseHelloError {
    Incomplete,
    Invalid,
}

fn parse_client_hello_sni(bytes: &[u8]) -> Result<String, ParseHelloError> {
    // A legal ClientHello may span several TLS handshake records. Reassemble
    // only the bounded handshake prefix required for SNI; socket bytes remain
    // untouched because the caller uses peek(2).
    let mut record_at = 0;
    let mut hello = Vec::new();
    loop {
        if bytes.len() < record_at + 5 {
            return Err(ParseHelloError::Incomplete);
        }
        if bytes[record_at] != 22 {
            return Err(ParseHelloError::Invalid);
        }
        let record_len = u16::from_be_bytes([bytes[record_at + 3], bytes[record_at + 4]]) as usize;
        if record_len == 0 || record_len > CLIENT_HELLO_LIMIT - 5 {
            return Err(ParseHelloError::Invalid);
        }
        let payload_at = record_at + 5;
        let record_end = payload_at
            .checked_add(record_len)
            .ok_or(ParseHelloError::Invalid)?;
        let payload = bytes
            .get(payload_at..record_end)
            .ok_or(ParseHelloError::Incomplete)?;
        if hello.len().saturating_add(payload.len()) > CLIENT_HELLO_LIMIT {
            return Err(ParseHelloError::Invalid);
        }
        hello.extend_from_slice(payload);
        if hello.len() >= 4 {
            let hello_len =
                ((hello[1] as usize) << 16) | ((hello[2] as usize) << 8) | hello[3] as usize;
            if hello_len > CLIENT_HELLO_LIMIT - 4 {
                return Err(ParseHelloError::Invalid);
            }
            if hello.len() >= 4 + hello_len {
                break;
            }
        }
        record_at = record_end;
    }
    if hello.len() < 4 || hello[0] != 1 {
        return Err(ParseHelloError::Invalid);
    }
    let hello_len = ((hello[1] as usize) << 16) | ((hello[2] as usize) << 8) | hello[3] as usize;
    if hello.len() < 4 + hello_len {
        return Err(ParseHelloError::Incomplete);
    }
    let mut at = 4;
    take(&hello, &mut at, 2 + 32)?; // legacy version and random
    let session_len = *take(&hello, &mut at, 1)?.first().unwrap() as usize;
    take(&hello, &mut at, session_len)?;
    let cipher_len = read_u16(&hello, &mut at)? as usize;
    if cipher_len == 0 || !cipher_len.is_multiple_of(2) {
        return Err(ParseHelloError::Invalid);
    }
    take(&hello, &mut at, cipher_len)?;
    let compression_len = *take(&hello, &mut at, 1)?.first().unwrap() as usize;
    take(&hello, &mut at, compression_len)?;
    let extensions_len = read_u16(&hello, &mut at)? as usize;
    let extensions = take(&hello, &mut at, extensions_len)?;
    let mut ext_at = 0;
    while ext_at < extensions.len() {
        let kind = read_u16(extensions, &mut ext_at)?;
        let len = read_u16(extensions, &mut ext_at)? as usize;
        let extension = take(extensions, &mut ext_at, len)?;
        if kind != 0 {
            continue;
        }
        let mut name_at = 0;
        let list_len = read_u16(extension, &mut name_at)? as usize;
        let names = take(extension, &mut name_at, list_len)?;
        let mut item_at = 0;
        while item_at < names.len() {
            let name_type = *take(names, &mut item_at, 1)?.first().unwrap();
            let name_len = read_u16(names, &mut item_at)? as usize;
            let name = take(names, &mut item_at, name_len)?;
            if name_type == 0 {
                let text = std::str::from_utf8(name).map_err(|_| ParseHelloError::Invalid)?;
                return normalize_dns_name(text).ok_or(ParseHelloError::Invalid);
            }
        }
        return Err(ParseHelloError::Invalid);
    }
    Err(ParseHelloError::Invalid)
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize) -> Result<&'a [u8], ParseHelloError> {
    let end = at.checked_add(count).ok_or(ParseHelloError::Invalid)?;
    let value = bytes.get(*at..end).ok_or(ParseHelloError::Incomplete)?;
    *at = end;
    Ok(value)
}

fn read_u16(bytes: &[u8], at: &mut usize) -> Result<u16, ParseHelloError> {
    let value = take(bytes, at, 2)?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use p256::ecdsa::{signature::Signer, SigningKey};
    use p256::elliptic_curve::rand_core::OsRng;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn extracts_sni_without_consuming_application_tls() {
        let hello = client_hello("desk-1234.relay.example.com");
        assert_eq!(
            parse_client_hello_sni(&hello).unwrap(),
            "desk-1234.relay.example.com"
        );
        for end in 0..hello.len() {
            assert_eq!(
                parse_client_hello_sni(&hello[..end]),
                Err(ParseHelloError::Incomplete)
            );
        }

        let payload = &hello[5..];
        let split = 23;
        let mut fragmented = Vec::new();
        for part in [&payload[..split], &payload[split..]] {
            fragmented.extend_from_slice(&[22, 3, 1]);
            fragmented.extend_from_slice(&(part.len() as u16).to_be_bytes());
            fragmented.extend_from_slice(part);
        }
        assert_eq!(
            parse_client_hello_sni(&fragmented).unwrap(),
            "desk-1234.relay.example.com",
        );
    }

    #[test]
    fn route_is_only_one_valid_label_below_public_domain() {
        assert_eq!(
            route_from_sni("desk-1234.relay.example.com", "relay.example.com"),
            Some("desk-1234")
        );
        assert_eq!(
            route_from_sni("relay.example.com", "relay.example.com"),
            None
        );
        assert_eq!(
            route_from_sni("bad.name.relay.example.com", "relay.example.com"),
            None
        );
        assert_eq!(
            route_from_sni("DESK-1234.relay.example.com", "relay.example.com"),
            None
        );
    }

    #[tokio::test]
    async fn phone_authority_signature_mints_one_route_and_persists_only_hashes() {
        let root = std::env::temp_dir().join(format!(
            "aiterm-relay-provision-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let state_file = root.join("provisioning.json");
        let control_origin = "https://control.relay.example.com:8443";
        let config = RelayConfig {
            connector_listen: "127.0.0.1:1".parse().unwrap(),
            ingress_listen: "127.0.0.1:2".parse().unwrap(),
            direct_listen: None,
            direct_public_host: None,
            direct_public_port: None,
            public_domain: "relay.example.com".into(),
            routes: Vec::new(),
            provisioning: Some(ProvisioningConfig {
                state_file: state_file.clone(),
                control_origin: control_origin.into(),
                connector_url: "wss://control.relay.example.com:8443/v1/connect".into(),
                public_port: 443,
                max_routes: 8,
                max_routes_per_ip: 2,
                max_routes_per_authority: 4,
            }),
        };
        let state = RelayState::from_config(&config).unwrap();
        let route_id = "desktop-1234567890abcdef";
        let token = "desktop-generated-connector-token-1234567890";
        let token_sha256: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let desktop_spki = [9u8; 32];
        let authority = SigningKey::random(&mut OsRng);
        let authority_public = authority.verifying_key().to_encoded_point(true);
        let digest = enrollment_digest(control_origin, route_id, &token_sha256, &desktop_spki);
        let signature: Signature = authority.sign(&digest);
        let request = ProvisionRouteRequest {
            route_id: route_id.into(),
            token_sha256: hex_bytes(&token_sha256),
            desktop_spki_sha256: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(desktop_spki),
            authority_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(authority_public.as_bytes()),
            signature_der: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(signature.to_der().as_bytes()),
        };
        let peer = "203.0.113.9:50000".parse().unwrap();
        let response = provision_route(
            State(state.clone()),
            ConnectInfo(peer),
            HeaderMap::new(),
            Json(request),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let provisioned: ProvisionedRouteView = serde_json::from_slice(&body).unwrap();
        assert!(provisioned
            .public_host
            .starts_with(&format!("{}.", provisioned.route_id)));
        assert_eq!(
            state
                .routes
                .read()
                .await
                .get(&provisioned.route_id)
                .unwrap()
                .token_sha256,
            token_sha256,
        );

        let persisted = std::fs::read_to_string(&state_file).unwrap();
        assert!(!persisted.contains(token));
        assert!(persisted.contains(&provisioned.route_id));

        let replay_signature: Signature = authority.sign(&digest);
        let replay = provision_route(
            State(state),
            ConnectInfo(peer),
            HeaderMap::new(),
            Json(ProvisionRouteRequest {
                route_id: route_id.into(),
                token_sha256: hex_bytes(&token_sha256),
                desktop_spki_sha256: base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(desktop_spki),
                authority_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(authority_public.as_bytes()),
                signature_der: base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(replay_signature.to_der().as_bytes()),
            }),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn phone_bytes_round_trip_without_consuming_the_tls_hello() {
        let token = "a-connector-token-that-is-long-enough";
        let token_sha256 = format!("{:x}", Sha256::digest(token.as_bytes()));
        let connector_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connector_addr = connector_listener.local_addr().unwrap();
        let ingress_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingress_addr = ingress_listener.local_addr().unwrap();
        let direct_listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let config = RelayConfig {
            connector_listen: connector_addr,
            ingress_listen: ingress_addr,
            direct_listen: None,
            direct_public_host: None,
            direct_public_port: None,
            public_domain: "relay.example.com".into(),
            provisioning: None,
            routes: vec![RouteConfig {
                id: "desk-1234".into(),
                token_sha256,
            }],
        };
        let state = RelayState::from_config(&config).unwrap();
        let server = tokio::spawn(run_with_listeners(
            state,
            connector_listener,
            ingress_listener,
            direct_listener,
        ));

        let mut request = format!("ws://{connector_addr}/v1/connect/desk-1234")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let (mut connector, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let hello = client_hello("desk-1234.relay.example.com");
        let mut phone = TcpStream::connect(ingress_addr).await.unwrap();
        phone.write_all(&hello).await.unwrap();

        let open = connector.next().await.unwrap().unwrap().into_data();
        let stream_id = match Frame::decode(&open).unwrap() {
            Frame::Open { stream_id } => stream_id,
            frame => panic!("expected open, got {frame:?}"),
        };
        let mut relayed = Vec::new();
        while relayed.len() < hello.len() {
            let bytes = connector.next().await.unwrap().unwrap().into_data();
            match Frame::decode(&bytes).unwrap() {
                Frame::Data {
                    stream_id: actual,
                    bytes,
                } => {
                    assert_eq!(actual, stream_id);
                    relayed.extend_from_slice(&bytes);
                }
                frame => panic!("expected data, got {frame:?}"),
            }
        }
        assert_eq!(
            relayed, hello,
            "SNI inspection must be a non-consuming peek"
        );

        let reply = b"opaque encrypted reply".to_vec();
        connector
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                Frame::Data {
                    stream_id,
                    bytes: reply.clone(),
                }
                .encode()
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let mut received = vec![0u8; reply.len()];
        phone.read_exact(&mut received).await.unwrap();
        assert_eq!(received, reply);

        server.abort();
    }

    #[tokio::test]
    async fn independent_routes_serve_clients_concurrently_without_crossover() {
        let token_a = "connector-token-for-desktop-a-123456789";
        let token_b = "connector-token-for-desktop-b-987654321";
        let connector_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connector_addr = connector_listener.local_addr().unwrap();
        let ingress_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingress_addr = ingress_listener.local_addr().unwrap();
        let direct_listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let config = RelayConfig {
            connector_listen: connector_addr,
            ingress_listen: ingress_addr,
            direct_listen: None,
            direct_public_host: None,
            direct_public_port: None,
            public_domain: "relay.example.com".into(),
            provisioning: None,
            routes: vec![
                RouteConfig {
                    id: "desktop-a".into(),
                    token_sha256: format!("{:x}", Sha256::digest(token_a.as_bytes())),
                },
                RouteConfig {
                    id: "desktop-b".into(),
                    token_sha256: format!("{:x}", Sha256::digest(token_b.as_bytes())),
                },
            ],
        };
        let state = RelayState::from_config(&config).unwrap();
        let server = tokio::spawn(run_with_listeners(
            state,
            connector_listener,
            ingress_listener,
            direct_listener,
        ));

        let (mut connector_a, _) =
            connect_test_connector(connector_addr, "desktop-a", token_a).await;
        let (mut connector_b, _) =
            connect_test_connector(connector_addr, "desktop-b", token_b).await;
        let (phone_a, phone_b) = tokio::join!(
            TcpStream::connect(ingress_addr),
            TcpStream::connect(ingress_addr)
        );
        let mut phone_a = phone_a.unwrap();
        let mut phone_b = phone_b.unwrap();
        let hello_a = client_hello("desktop-a.relay.example.com");
        let hello_b = client_hello("desktop-b.relay.example.com");
        let (write_a, write_b) =
            tokio::join!(phone_a.write_all(&hello_a), phone_b.write_all(&hello_b));
        write_a.unwrap();
        write_b.unwrap();

        let ((stream_a, relayed_a), (stream_b, relayed_b)) = tokio::join!(
            read_test_stream(&mut connector_a, hello_a.len()),
            read_test_stream(&mut connector_b, hello_b.len())
        );
        assert_ne!(stream_a, stream_b);
        assert_eq!(relayed_a, hello_a);
        assert_eq!(relayed_b, hello_b);

        send_test_reply(&mut connector_a, stream_a, b"reply from desktop a").await;
        send_test_reply(&mut connector_b, stream_b, b"reply from desktop b").await;
        let mut reply_a = vec![0; b"reply from desktop a".len()];
        let mut reply_b = vec![0; b"reply from desktop b".len()];
        let (read_a, read_b) = tokio::join!(
            phone_a.read_exact(&mut reply_a),
            phone_b.read_exact(&mut reply_b)
        );
        read_a.unwrap();
        read_b.unwrap();
        assert_eq!(reply_a, b"reply from desktop a");
        assert_eq!(reply_b, b"reply from desktop b");

        server.abort();
    }

    #[tokio::test]
    async fn direct_rendezvous_requires_role_cookies_and_returns_observed_addresses() {
        let config = RelayConfig {
            connector_listen: "127.0.0.1:1".parse().unwrap(),
            ingress_listen: "127.0.0.1:2".parse().unwrap(),
            direct_listen: None,
            direct_public_host: None,
            direct_public_port: None,
            public_domain: "relay.example.com".into(),
            provisioning: None,
            routes: Vec::new(),
        };
        let state = RelayState::from_config(&config).unwrap();
        let id = [7; aiterm_relay_protocol::DIRECT_ID_BYTES];
        let desktop_cookie = [8; aiterm_relay_protocol::DIRECT_COOKIE_BYTES];
        let phone_cookie = [9; aiterm_relay_protocol::DIRECT_COOKIE_BYTES];
        state.direct.lock().await.insert(
            id,
            DirectRendezvous {
                route: "desktop-test".into(),
                desktop_cookie,
                phone_cookie,
                desktop: None,
                phone: None,
                created: Instant::now(),
            },
        );
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_direct(listener, state));
        let desktop = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let phone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let desktop_address = desktop.local_addr().unwrap();
        let phone_address = phone.local_addr().unwrap();
        let mut bytes = [0u8; aiterm_relay_protocol::MAX_DIRECT_PACKET_BYTES];

        phone
            .send_to(
                &DirectPacket::BindPhone {
                    id,
                    cookie: [0; aiterm_relay_protocol::DIRECT_COOKIE_BYTES],
                }
                .encode(),
                relay,
            )
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(30), phone.recv_from(&mut bytes))
                .await
                .is_err()
        );

        desktop
            .send_to(
                &DirectPacket::BindDesktop {
                    id,
                    cookie: desktop_cookie,
                }
                .encode(),
                relay,
            )
            .await
            .unwrap();
        let (count, _) = desktop.recv_from(&mut bytes).await.unwrap();
        assert_eq!(
            DirectPacket::decode(&bytes[..count]).unwrap(),
            DirectPacket::Bound { id }
        );
        phone
            .send_to(
                &DirectPacket::BindPhone {
                    id,
                    cookie: phone_cookie,
                }
                .encode(),
                relay,
            )
            .await
            .unwrap();
        let (count, _) = phone.recv_from(&mut bytes).await.unwrap();
        assert_eq!(
            DirectPacket::decode(&bytes[..count]).unwrap(),
            DirectPacket::Bound { id }
        );
        let (count, _) = phone.recv_from(&mut bytes).await.unwrap();
        assert_eq!(
            DirectPacket::decode(&bytes[..count]).unwrap(),
            DirectPacket::Peer {
                id,
                address: desktop_address,
            }
        );
        let (count, _) = desktop.recv_from(&mut bytes).await.unwrap();
        assert_eq!(
            DirectPacket::decode(&bytes[..count]).unwrap(),
            DirectPacket::Peer {
                id,
                address: phone_address,
            }
        );

        server.abort();
    }

    async fn connect_test_connector(
        connector_addr: SocketAddr,
        route: &str,
        token: &str,
    ) -> (
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ) {
        let mut request = format!("ws://{connector_addr}/v1/connect/{route}")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        tokio_tungstenite::connect_async(request).await.unwrap()
    }

    async fn read_test_stream(
        connector: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<TcpStream>,
        >,
        expected_len: usize,
    ) -> (u64, Vec<u8>) {
        let open = connector.next().await.unwrap().unwrap().into_data();
        let stream_id = match Frame::decode(&open).unwrap() {
            Frame::Open { stream_id } => stream_id,
            frame => panic!("expected open, got {frame:?}"),
        };
        let mut relayed = Vec::new();
        while relayed.len() < expected_len {
            let message = connector.next().await.unwrap().unwrap().into_data();
            match Frame::decode(&message).unwrap() {
                Frame::Data {
                    stream_id: actual,
                    bytes,
                } => {
                    assert_eq!(actual, stream_id);
                    relayed.extend_from_slice(&bytes);
                }
                frame => panic!("expected data, got {frame:?}"),
            }
        }
        (stream_id, relayed)
    }

    async fn send_test_reply(
        connector: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<TcpStream>,
        >,
        stream_id: u64,
        reply: &[u8],
    ) {
        connector
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                Frame::Data {
                    stream_id,
                    bytes: reply.to_vec(),
                }
                .encode()
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
    }

    fn client_hello(host: &str) -> Vec<u8> {
        let mut names = vec![0];
        names.extend_from_slice(&(host.len() as u16).to_be_bytes());
        names.extend_from_slice(host.as_bytes());
        let mut sni = Vec::new();
        sni.extend_from_slice(&(names.len() as u16).to_be_bytes());
        sni.extend_from_slice(&names);
        let mut extensions = Vec::new();
        extensions.extend_from_slice(&0u16.to_be_bytes());
        extensions.extend_from_slice(&(sni.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&sni);
        let mut body = Vec::new();
        body.extend_from_slice(&[3, 3]);
        body.extend_from_slice(&[0; 32]);
        body.push(0);
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x13, 0x01]);
        body.push(1);
        body.push(0);
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);
        let mut handshake = vec![1, 0, 0, body.len() as u8];
        handshake.extend_from_slice(&body);
        let mut record = vec![22, 3, 1];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }
}
