use super::auth::{set_private_permissions, write_private_file};
use aiterm_relay_protocol::{enrollment_digest, Frame};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;

const CONFIG_FILE: &str = "relay.json";
const SERVER_CONFIG_FILE: &str = "relay-server.json";
const MAX_STREAMS: usize = 128;
const STREAM_QUEUE: usize = 32;
const OUTGOING_QUEUE: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PROVISION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROVISION_RESPONSE_BYTES: u64 = 8 * 1024;

/// The control plane selected for the next managed relay enrollment. Keeping
/// its public domain beside the origin lets the gateway issue a matching TLS
/// certificate before the relay route itself exists, without making LAN start
/// depend on the relay being online every time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayServerConfig {
    pub control_origin: String,
    pub public_domain: String,
}

impl RelayServerConfig {
    pub fn known(control_origin: &str, public_domain: &str) -> Result<Self, String> {
        let origin = validated_server_origin(control_origin)?;
        let public_domain = normalize_dns_name(public_domain)
            .filter(|value| value == public_domain)
            .ok_or_else(|| "relay public domain is invalid".to_string())?;
        Ok(Self {
            control_origin: origin.as_str().trim_end_matches('/').to_string(),
            public_domain,
        })
    }

    pub async fn discover(server: &str) -> Result<Self, String> {
        let (control_origin, info) = fetch_relay_info(server).await?;
        Self::known(&control_origin, &info.public_domain)
    }

    pub fn from_route(config: &RelayConfig) -> Option<Self> {
        let mut origin = url::Url::parse(&config.connector_url).ok()?;
        if !matches!(origin.path(), "/v1/connect" | "/v1/connect/") {
            return None;
        }
        origin
            .set_scheme(if origin.scheme() == "wss" { "https" } else { "http" })
            .ok()?;
        origin.set_path("/");
        let public_domain = config
            .public_host
            .strip_prefix(&format!("{}.", config.route_id))?;
        Self::known(origin.as_str(), public_domain).ok()
    }

    pub fn load(root: &Path) -> Result<Option<Self>, String> {
        let path = root.join(SERVER_CONFIG_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read relay server setting: {error}")),
        };
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("relay server setting is corrupt: {error}"))?;
        Self::known(&config.control_origin, &config.public_domain).map(Some)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let validated = Self::known(&self.control_origin, &self.public_domain)?;
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        set_private_permissions(root, 0o700).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&validated).map_err(|error| error.to_string())?;
        let destination = root.join(SERVER_CONFIG_FILE);
        let temporary = destination.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        if let Err(error) = write_private_file(&temporary, &bytes)
            .and_then(|()| std::fs::rename(&temporary, &destination))
        {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct RelayEnrollmentDraft {
    control_origin: String,
    config: RelayConfig,
    token_sha256: [u8; 32],
    desktop_spki_sha256: [u8; 32],
    authorization_digest: [u8; 32],
}

impl std::fmt::Debug for RelayEnrollmentDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RelayEnrollmentDraft")
            .field("control_origin", &self.control_origin)
            .field("public_host", &self.config.public_host)
            .field("route_id", &self.config.route_id)
            .finish_non_exhaustive()
    }
}

impl RelayEnrollmentDraft {
    pub fn authorization_digest(&self) -> &[u8; 32] {
        &self.authorization_digest
    }

    pub fn public_endpoint(&self) -> (&str, u16) {
        (&self.config.public_host, self.config.public_port)
    }

    pub async fn register(
        &self,
        authority_public_key: &[u8],
        signature_der: &[u8],
    ) -> Result<RelayConfig, String> {
        if authority_public_key.len() != 33 || !(8..=80).contains(&signature_der.len()) {
            return Err("the phone returned an invalid relay authorization".into());
        }
        let mut endpoint = validated_server_origin(&self.control_origin)?;
        endpoint.set_path("/v1/provision");
        let request = ProvisionRouteRequest {
            route_id: &self.config.route_id,
            token_sha256: hex_bytes(&self.token_sha256),
            desktop_spki_sha256: URL_SAFE_NO_PAD.encode(self.desktop_spki_sha256),
            authority_public_key: URL_SAFE_NO_PAD.encode(authority_public_key),
            signature_der: URL_SAFE_NO_PAD.encode(signature_der),
        };
        let response = provisioning_client()?
            .post(endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|_| "the relay server could not be reached".to_string())?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                404 => "this relay server does not support automatic setup".into(),
                409 => "the relay route was already claimed".into(),
                429 => "this phone or network has reached the relay route limit".into(),
                _ => format!("relay setup failed with status {}", response.status().as_u16()),
            });
        }
        let provisioned: ProvisionedRelay = bounded_json(response).await?;
        if provisioned.connector_url != self.config.connector_url
            || provisioned.public_host != self.config.public_host
            || provisioned.public_port != self.config.public_port
            || provisioned.route_id != self.config.route_id
        {
            return Err("the relay server returned a different route than the phone authorized".into());
        }
        self.config.validate()?;
        Ok(self.config.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub connector_url: String,
    pub public_host: String,
    pub public_port: u16,
    pub route_id: String,
    pub token: String,
    #[serde(default)]
    pub managed: bool,
}

impl RelayConfig {
    pub fn validate(&self) -> Result<(), String> {
        let url = url::Url::parse(&self.connector_url)
            .map_err(|_| "relay connector URL is invalid".to_string())?;
        if !matches!(url.scheme(), "ws" | "wss")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("relay connector URL must be a ws:// or wss:// URL without credentials, query, or fragment".into());
        }
        if url.scheme() == "ws"
            && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
        {
            return Err("a non-local relay connector must use wss://".into());
        }
        if normalize_dns_name(&self.public_host).as_deref() != Some(self.public_host.as_str()) {
            return Err("relay public host must be a lowercase DNS name".into());
        }
        if self.public_port == 0 {
            return Err("relay public port is invalid".into());
        }
        if !valid_route_id(&self.route_id) {
            return Err("relay route id is invalid".into());
        }
        if !self.public_host.starts_with(&format!("{}.", self.route_id)) {
            return Err("relay public host must begin with the route id".into());
        }
        if !(43..=128).contains(&self.token.len())
            || !self.token.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("relay connector token is invalid".into());
        }
        Ok(())
    }

    fn websocket_url(&self) -> String {
        format!(
            "{}/{}",
            self.connector_url.trim_end_matches('/'),
            self.route_id
        )
    }

    pub fn load(root: &Path) -> Result<Option<Self>, String> {
        Self::load_named(root, CONFIG_FILE)
    }

    /// The same file shape under another name — the phone listener keeps its
    /// own route beside the gateway's `relay.json`.
    pub fn load_named(root: &Path, file: &str) -> Result<Option<Self>, String> {
        let path = root.join(file);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read relay settings: {error}")),
        };
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("relay settings are corrupt: {error}"))?;
        config.validate()?;
        Ok(Some(config))
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        self.save_named(root, CONFIG_FILE)
    }

    pub fn save_named(&self, root: &Path, file: &str) -> Result<(), String> {
        self.validate()?;
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        set_private_permissions(root, 0o700).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        let destination = root.join(file);
        let temporary = destination.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        if let Err(error) = write_private_file(&temporary, &bytes)
            .and_then(|()| std::fs::rename(&temporary, &destination))
        {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        Ok(())
    }

    pub async fn prepare_enrollment(
        server: &str,
        desktop_spki_fingerprint: &str,
    ) -> Result<RelayEnrollmentDraft, String> {
        let (control_origin, info) = fetch_relay_info(server).await?;
        let public_domain = normalize_dns_name(&info.public_domain)
            .filter(|value| value == &info.public_domain)
            .ok_or_else(|| "the relay server returned an invalid public domain".to_string())?;
        let decoded_spki = URL_SAFE_NO_PAD.decode(desktop_spki_fingerprint.as_bytes())
            .map_err(|_| "the desktop identity fingerprint is invalid".to_string())?;
        let desktop_spki_sha256: [u8; 32] = decoded_spki.try_into()
            .map_err(|_| "the desktop identity fingerprint is invalid".to_string())?;
        let mut route_bytes = [0u8; 12];
        let mut token_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut route_bytes);
        OsRng.fill_bytes(&mut token_bytes);
        let route_id = format!("desktop-{}", hex_bytes(&route_bytes));
        let token = URL_SAFE_NO_PAD.encode(token_bytes);
        let token_sha256: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let config = Self {
            connector_url: info.connector_url,
            public_host: format!("{route_id}.{public_domain}"),
            public_port: info.public_port,
            route_id,
            token,
            managed: true,
        };
        config.validate()?;
        let authorization_digest = enrollment_digest(
            &control_origin,
            &config.route_id,
            &token_sha256,
            &desktop_spki_sha256,
        );
        Ok(RelayEnrollmentDraft {
            control_origin,
            config,
            token_sha256,
            desktop_spki_sha256,
            authorization_digest,
        })
    }

    pub async fn deprovision(&self) -> Result<(), String> {
        if !self.managed {
            return Ok(());
        }
        let mut url = url::Url::parse(&self.connector_url)
            .map_err(|_| "stored relay connector URL is invalid".to_string())?;
        url.set_scheme(if url.scheme() == "wss" { "https" } else { "http" })
            .map_err(|_| "stored relay connector URL is invalid".to_string())?;
        url.set_path(&format!("/v1/routes/{}", self.route_id));
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(PROVISION_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not initialize relay removal".to_string())?;
        let response = client.delete(url).bearer_auth(&self.token).send().await
            .map_err(|_| "the relay server could not be reached".to_string())?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(format!("relay removal failed with status {}", response.status().as_u16()))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionedRelay {
    connector_url: String,
    public_host: String,
    public_port: u16,
    route_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayInfo {
    control_origin: String,
    connector_url: String,
    public_domain: String,
    public_port: u16,
}

async fn fetch_relay_info(server: &str) -> Result<(String, RelayInfo), String> {
    let base = validated_server_origin(server)?;
    let control_origin = base.as_str().trim_end_matches('/').to_string();
    let mut info_url = base;
    info_url.set_path("/v1/info");
    let response = provisioning_client()?
        .get(info_url)
        .send()
        .await
        .map_err(|_| "the relay server could not be reached".to_string())?;
    if !response.status().is_success() {
        return Err("this relay server does not support automatic setup".into());
    }
    let info: RelayInfo = bounded_json(response).await?;
    let returned_origin = validated_server_origin(&info.control_origin)?;
    if returned_origin.as_str().trim_end_matches('/') != control_origin {
        return Err("the relay server returned an unexpected control identity".into());
    }
    Ok((control_origin, info))
}

#[derive(Serialize)]
struct ProvisionRouteRequest<'a> {
    route_id: &'a str,
    token_sha256: String,
    desktop_spki_sha256: String,
    authority_public_key: String,
    signature_der: String,
}

fn validated_server_origin(server: &str) -> Result<url::Url, String> {
    let base = url::Url::parse(server).map_err(|_| "relay server URL is invalid".to_string())?;
    let local_http = base.scheme() == "http"
        && matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !(base.scheme() == "https" || local_http)
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || !matches!(base.path(), "" | "/")
    {
        return Err("relay server must be an https:// origin without credentials or a path".into());
    }
    Ok(base)
}

fn provisioning_client() -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(PROVISION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "could not initialize relay provisioning".to_string())
}

async fn bounded_json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    if response.content_length().is_some_and(|length| length > MAX_PROVISION_RESPONSE_BYTES) {
        return Err("the relay server returned an oversized setup response".into());
    }
    let bytes = response.bytes().await
        .map_err(|_| "the relay setup response could not be read".to_string())?;
    if bytes.len() as u64 > MAX_PROVISION_RESPONSE_BYTES {
        return Err("the relay server returned an oversized setup response".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "the relay server returned an invalid setup response".to_string())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayConnectionState {
    #[default]
    Off,
    Connecting,
    Connected,
    Retrying,
}

pub struct RelayConnectorHandle {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    state: watch::Receiver<RelayConnectionState>,
}

impl RelayConnectorHandle {
    pub fn start(config: RelayConfig, local: SocketAddr) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (state_tx, state_rx) = watch::channel(RelayConnectionState::Connecting);
        let task = tokio::spawn(run(config, local, shutdown_rx, state_tx));
        Self {
            shutdown: Some(shutdown_tx),
            task,
            state: state_rx,
        }
    }

    pub fn state(&self) -> RelayConnectionState {
        *self.state.borrow()
    }

    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

async fn run(
    config: RelayConfig,
    local: SocketAddr,
    mut shutdown: oneshot::Receiver<()>,
    state: watch::Sender<RelayConnectionState>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let _ = state.send(if backoff == Duration::from_secs(1) {
            RelayConnectionState::Connecting
        } else {
            RelayConnectionState::Retrying
        });
        let connected = tokio::select! {
            _ = &mut shutdown => break,
            result = connect_once(&config, local, &state) => result,
        };
        if let Err(error) = connected {
            crate::diag!("remote", "relay connector: {error}");
        }
        let was_connected = *state.borrow() == RelayConnectionState::Connected;
        let _ = state.send(RelayConnectionState::Retrying);
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = if was_connected {
            Duration::from_secs(1)
        } else {
            (backoff * 2).min(Duration::from_secs(30))
        };
    }
    let _ = state.send(RelayConnectionState::Off);
}

async fn connect_once(
    config: &RelayConfig,
    local: SocketAddr,
    state: &watch::Sender<RelayConnectionState>,
) -> Result<(), String> {
    let mut request = config
        .websocket_url()
        .into_client_request()
        .map_err(|error| format!("invalid connector request: {error}"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {}", config.token)
            .parse()
            .map_err(|_| "invalid connector token".to_string())?,
    );
    let (socket, _) =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .map_err(|_| "connection timed out".to_string())?
            .map_err(|error| format!("connection failed: {error}"))?;
    let _ = state.send(RelayConnectionState::Connected);
    let (mut sink, mut source) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Frame>(OUTGOING_QUEUE);
    let (done_tx, mut done_rx) = mpsc::channel::<u64>(MAX_STREAMS);
    let mut streams = HashMap::<u64, mpsc::Sender<Vec<u8>>>::new();
    let mut tasks = JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let result = loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if send_frame(&mut sink, Frame::Ping).await.is_err() { break Err("relay heartbeat failed".into()); }
            }
            Some(stream_id) = done_rx.recv() => { streams.remove(&stream_id); }
            outbound = outgoing_rx.recv() => {
                let Some(frame) = outbound else { break Err("relay writer stopped".into()) };
                if send_frame(&mut sink, frame).await.is_err() { break Err("relay write failed".into()); }
            }
            inbound = source.next() => {
                let Some(Ok(message)) = inbound else { break Err("relay connection closed".into()) };
                let Message::Binary(bytes) = message else {
                    if matches!(message, Message::Close(_)) { break Err("relay connection closed".into()); }
                    continue;
                };
                let frame = Frame::decode(&bytes).map_err(|error| error.to_string())?;
                match frame {
                    Frame::Open { stream_id } => {
                        if streams.contains_key(&stream_id) || streams.len() >= MAX_STREAMS {
                            let _ = outgoing_tx.send(Frame::Close { stream_id, reason: b"stream limit".to_vec() }).await;
                            continue;
                        }
                        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
                        streams.insert(stream_id, tx);
                        let outgoing = outgoing_tx.clone();
                        let done = done_tx.clone();
                        tasks.spawn(async move { bridge_local(stream_id, local, rx, outgoing, done).await });
                    }
                    Frame::Data { stream_id, bytes } => {
                        let Some(stream) = streams.get(&stream_id).cloned() else { continue };
                        if stream.try_send(bytes).is_err() {
                            streams.remove(&stream_id);
                            let _ = outgoing_tx.try_send(Frame::Close {
                                stream_id,
                                reason: b"desktop too slow".to_vec(),
                            });
                        }
                    }
                    Frame::Close { stream_id, .. } => { streams.remove(&stream_id); }
                    Frame::Ping => { let _ = outgoing_tx.send(Frame::Pong).await; }
                    Frame::Pong => {}
                }
            }
        }
    };
    streams.clear();
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}

async fn send_frame<S>(sink: &mut S, frame: Frame) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let bytes = frame.encode().map_err(|error| error.to_string())?;
    sink.send(Message::Binary(bytes.into()))
        .await
        .map_err(|error| error.to_string())
}

async fn bridge_local(
    stream_id: u64,
    local: SocketAddr,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    outgoing: mpsc::Sender<Frame>,
    done: mpsc::Sender<u64>,
) {
    let socket = match tokio::time::timeout(LOCAL_CONNECT_TIMEOUT, TcpStream::connect(local)).await
    {
        Ok(Ok(socket)) => socket,
        _ => {
            let _ = outgoing
                .send(Frame::Close {
                    stream_id,
                    reason: b"desktop unavailable".to_vec(),
                })
                .await;
            let _ = done.send(stream_id).await;
            return;
        }
    };
    let (mut reader, mut writer) = socket.into_split();
    let mut buffer = vec![0u8; aiterm_relay_protocol::MAX_DATA_BYTES];
    loop {
        tokio::select! {
            read = reader.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if outgoing.send(Frame::Data { stream_id, bytes: buffer[..count].to_vec() }).await.is_err() { break; }
                }
            },
            bytes = inbound.recv() => match bytes {
                Some(bytes) if writer.write_all(&bytes).await.is_ok() => {}
                _ => break,
            }
        }
    }
    let _ = outgoing
        .send(Frame::Close {
            stream_id,
            reason: Vec::new(),
        })
        .await;
    let _ = done.send(stream_id).await;
}

fn normalize_dns_name(value: &str) -> Option<String> {
    let normalized = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 253
        || normalized.split('.').any(|label| {
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
        Some(normalized)
    }
}

fn valid_route_id(value: &str) -> bool {
    (8..=63).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};
    use p256::elliptic_curve::rand_core::OsRng;
    use std::fs;

    fn valid_config() -> RelayConfig {
        RelayConfig {
            connector_url: "wss://control.relay.example.com/v1/connect".into(),
            public_host: "desk-1234.relay.example.com".into(),
            public_port: 443,
            route_id: "desk-1234".into(),
            token: "x".repeat(43),
            managed: false,
        }
    }

    #[test]
    fn config_rejects_ambiguous_or_unsafe_endpoints() {
        assert!(valid_config().validate().is_ok());
        let mut value = valid_config();
        value.connector_url = "https://relay.example.com".into();
        assert!(value.validate().is_err());
        let mut value = valid_config();
        value.connector_url = "ws://relay.example.com/v1/connect".into();
        assert!(value.validate().is_err());
        let mut value = valid_config();
        value.connector_url = "ws://127.0.0.1:8080/v1/connect".into();
        assert!(value.validate().is_ok());
        let mut value = valid_config();
        value.public_host = "Desk.relay.example.com".into();
        assert!(value.validate().is_err());
        let mut value = valid_config();
        value.token = "short".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn persisted_config_round_trips_without_exposing_a_view() {
        let root =
            std::env::temp_dir().join(format!("aiterm-relay-config-{}", uuid::Uuid::new_v4()));
        let config = valid_config();
        config.save(&root).unwrap();
        assert_eq!(RelayConfig::load(&root).unwrap(), Some(config));
        let server = RelayServerConfig::known(
            "https://control.relay.example.com/",
            "relay.example.com",
        )
        .unwrap();
        server.save(&root).unwrap();
        assert_eq!(RelayServerConfig::load(&root).unwrap(), Some(server));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_route_recovers_its_server_for_upgrade() {
        assert_eq!(
            RelayServerConfig::from_route(&valid_config()),
            Some(RelayServerConfig {
                control_origin: "https://control.relay.example.com".into(),
                public_domain: "relay.example.com".into(),
            })
        );
    }

    #[test]
    fn server_setting_accepts_only_a_secure_origin_and_plain_dns_domain() {
        assert!(RelayServerConfig::known(
            "https://control.relay.example.com:8443",
            "relay.example.com",
        ).is_ok());
        assert!(RelayServerConfig::known(
            "https://user@control.relay.example.com",
            "relay.example.com",
        ).is_err());
        assert!(RelayServerConfig::known(
            "https://control.relay.example.com/a/path",
            "relay.example.com",
        ).is_err());
        assert!(RelayServerConfig::known(
            "https://control.relay.example.com",
            "*.relay.example.com",
        ).is_err());
    }

    #[tokio::test]
    async fn phone_signed_provisioning_keeps_the_connector_secret_on_the_desktop() {
        #[derive(Clone)]
        struct TestServer {
            origin: String,
            connector_url: String,
        }
        async fn info(State(state): State<TestServer>) -> Json<serde_json::Value> {
            Json(serde_json::json!({
                "control_origin": state.origin,
                "connector_url": state.connector_url,
                "public_domain": "relay.example.com",
                "public_port": 443,
            }))
        }
        async fn provision(
            State(state): State<TestServer>,
            Json(request): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let route = request["route_id"].as_str().unwrap();
            Json(serde_json::json!({
                "connector_url": state.connector_url,
                "public_host": format!("{route}.relay.example.com"),
                "public_port": 443,
                "route_id": route,
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let test_state = TestServer {
            origin: origin.clone(),
            connector_url: format!("ws://{address}/v1/connect"),
        };
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new()
                .route("/v1/info", get(info))
                .route("/v1/provision", post(provision))
                .with_state(test_state))
                .await
                .unwrap();
        });

        let selected = RelayServerConfig::discover(&origin).await.unwrap();
        assert_eq!(selected.control_origin, origin);
        assert_eq!(selected.public_domain, "relay.example.com");
        let desktop_fingerprint = URL_SAFE_NO_PAD.encode([9u8; 32]);
        let draft = RelayConfig::prepare_enrollment(&origin, &desktop_fingerprint).await.unwrap();
        let authority = SigningKey::random(&mut OsRng);
        let signature: Signature = authority.sign(draft.authorization_digest());
        let config = draft.register(
            authority.verifying_key().to_encoded_point(true).as_bytes(),
            signature.to_der().as_bytes(),
        ).await.unwrap();
        assert_eq!(config.token.len(), 43);
        assert!(config.validate().is_ok());
        server.abort();
    }

    #[test]
    fn a_saved_config_is_replaced_whole_and_leaves_no_temporary_behind() {
        let root = std::env::temp_dir().join(format!("aiterm-relay-save-{}", uuid::Uuid::new_v4()));
        let first = RelayConfig::load(&root).unwrap();
        assert!(first.is_none());
        let config = valid_config();
        config.save(&root).unwrap();
        config.save(&root).unwrap();
        assert_eq!(RelayConfig::load(&root).unwrap(), Some(config));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
