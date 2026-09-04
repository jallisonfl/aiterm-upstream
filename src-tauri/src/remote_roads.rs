//! Roads for the phone listener — the pieces of `remote_api` that decide
//! *which* addresses a phone is told about and *what* a VPN looks like
//! from here. See `docs/remote/remote-roads.md`.
//!
//! A road is one way a phone reaches this desktop: `lan` (a private address
//! on the same network), `vpn` (Tailscale, WireGuard, anything that leaves
//! a 100.64/10 or fc00::/7 address, or an interface named like a tunnel),
//! `relay` (the AITerm relay, enrolled the way the gateway's route is) and
//! `iroh`. Roads are independent; a road that is off contributes nothing to
//! the QR and nothing to `/v1/status`.
//!
//! Everything here is pure or cheap except the Tailscale question, which
//! shells out and is cached — the status command must never wait on it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Which road an address belongs to. `Vpn` carries its flavour for the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Road {
    Lan,
    Vpn(VpnKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VpnKind {
    Tailscale,
    Wireguard,
    Other,
}

impl VpnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VpnKind::Tailscale => "tailscale",
            VpnKind::Wireguard => "wireguard",
            VpnKind::Other => "other",
        }
    }
}

/// What the panel shows about the VPN road.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct VpnStatus {
    pub detected: bool,
    /// "tailscale" | "wireguard" | "other"
    pub kind: Option<String>,
    pub interface: Option<String>,
    pub address: Option<String>,
    /// Tailscale's MagicDNS name for this machine, when the CLI answers.
    pub magic_dns: Option<String>,
}

/// Carrier-grade NAT space, which Tailscale hands out: 100.64.0.0/10.
pub fn is_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// IPv6 unique local addresses, fc00::/7 — what Tailscale and most private
/// overlays use for their v6 side.
pub fn is_ula(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

fn is_rfc1918(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 10 || (o[0] == 192 && o[1] == 168) || (o[0] == 172 && (16..32).contains(&o[1]))
}

/// Does the interface's name say "tunnel"? Tailscale is `tailscale0`,
/// WireGuard is `wg0`/`wg-home`, and utun is what macOS-style userland
/// tunnels (and some VPN clients on Linux) call theirs.
fn vpn_kind_of_interface(name: &str) -> Option<VpnKind> {
    if name.starts_with("tailscale") {
        Some(VpnKind::Tailscale)
    } else if name.starts_with("wg") {
        Some(VpnKind::Wireguard)
    } else if name.starts_with("utun") {
        Some(VpnKind::Other)
    } else {
        None
    }
}

/// Interfaces a phone can never reach: container bridges and virtual
/// machine networks live inside this box.
fn is_container_interface(name: &str) -> bool {
    name.starts_with("docker") || name.starts_with("br-") || name.starts_with("virbr") || name.starts_with("veth")
}

/// Which road one address on one interface belongs to, or `None` when it is
/// not something a phone could dial (loopback, link-local, a bridge).
///
/// The address wins over the interface for Tailscale — a 100.64/10 address
/// is Tailscale wherever it sits — and the interface wins for everything
/// else: a WireGuard tunnel usually carries an ordinary RFC1918 address
/// that only the name tells apart from the LAN.
pub fn classify(interface: &str, ip: IpAddr) -> Option<Road> {
    if is_container_interface(interface) {
        return None;
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
                return None;
            }
            if is_cgnat(v4) {
                return Some(Road::Vpn(VpnKind::Tailscale));
            }
            if let Some(kind) = vpn_kind_of_interface(interface) {
                return Some(Road::Vpn(kind));
            }
            // A public v4 on a plain interface is still the direct road.
            Some(Road::Lan)
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return None;
            }
            if let Some(kind) = vpn_kind_of_interface(interface) {
                return Some(Road::Vpn(kind));
            }
            if is_ula(v6) {
                return Some(Road::Vpn(VpnKind::Other));
            }
            Some(Road::Lan)
        }
    }
}

/// One address as `if_addrs` hands it over, reduced to what classification
/// needs. Kept as a plain pair so tests can feed a fake interface table.
pub type Found = (String, IpAddr);

fn found_addresses() -> Vec<Found> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| (i.name.clone(), i.ip()))
        .collect()
}

/// The ordered host list a phone is told about, honouring which roads are
/// on. Tailscale first (a phone on the tailnet shares it from anywhere),
/// then other tunnels, then the LAN, then anything else. IPv4 only, as before: the
/// phone builds `https://<h>:<port>` from the bare host and a v6 literal
/// would need brackets on both sides — v6 is classified for *detection*
/// (below) but not advertised.
pub fn advertised(lan: bool, vpn: bool) -> Vec<String> {
    advertised_from(found_addresses(), lan, vpn)
}

pub fn advertised_from(found: Vec<Found>, lan: bool, vpn: bool) -> Vec<String> {
    let mut out: Vec<(u8, String)> = found
        .into_iter()
        .filter_map(|(name, ip)| {
            let IpAddr::V4(v4) = ip else { return None };
            let rank = match classify(&name, ip)? {
                Road::Vpn(VpnKind::Tailscale) if vpn => 0,
                Road::Vpn(_) if vpn => 1,
                Road::Lan if lan && is_rfc1918(v4) => 2,
                Road::Lan if lan => 3,
                _ => return None,
            };
            Some((rank, v4.to_string()))
        })
        .collect();
    out.sort();
    out.dedup();
    out.into_iter().map(|(_, a)| a).collect()
}

/// What the VPN road looks like right now: the first tunnel address found,
/// Tailscale preferred, plus the MagicDNS name when the CLI answers.
pub fn vpn_status() -> VpnStatus {
    let mut status = vpn_status_from(found_addresses());
    if status.detected {
        status.magic_dns = magic_dns_cached();
    }
    status
}

pub fn vpn_status_from(found: Vec<Found>) -> VpnStatus {
    let mut best: Option<(u8, VpnKind, String, IpAddr)> = None;
    for (name, ip) in found {
        let Some(Road::Vpn(kind)) = classify(&name, ip) else { continue };
        // Tailscale first, then WireGuard, then the rest; v4 before v6 so
        // the panel shows the address the phone will actually dial.
        let rank = match kind {
            VpnKind::Tailscale => 0,
            VpnKind::Wireguard => 2,
            VpnKind::Other => 4,
        } + if ip.is_ipv6() { 1 } else { 0 };
        if best.as_ref().is_none_or(|(r, ..)| rank < *r) {
            best = Some((rank, kind, name, ip));
        }
    }
    match best {
        Some((_, kind, name, ip)) => VpnStatus {
            detected: true,
            kind: Some(kind.as_str().into()),
            interface: Some(name),
            address: Some(ip.to_string()),
            magic_dns: None,
        },
        None => VpnStatus::default(),
    }
}

const MAGIC_DNS_TTL: Duration = Duration::from_secs(30);
const MAGIC_DNS_TIMEOUT: Duration = Duration::from_secs(2);

static MAGIC_DNS: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);

/// `tailscale status --json` → `.Self.DNSName`, trailing dot trimmed. Cached
/// for 30 s so a panel polling every second asks the CLI twice a minute,
/// and bounded to 2 s so a wedged tailscaled cannot stall the status call.
fn magic_dns_cached() -> Option<String> {
    if let Some((at, value)) = MAGIC_DNS.lock().unwrap().clone() {
        if at.elapsed() < MAGIC_DNS_TTL {
            return value;
        }
    }
    let value = magic_dns_query();
    *MAGIC_DNS.lock().unwrap() = Some((Instant::now(), value.clone()));
    value
}

fn magic_dns_query() -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("tailscale")
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) if started.elapsed() < MAGIC_DNS_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let json: serde_json::Value = serde_json::from_str(&out).ok()?;
    let name = json.get("Self")?.get("DNSName")?.as_str()?.trim_end_matches('.').trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

// ---------------------------------------------------------------- QR fields

/// The phone-listener's fields for a pairing QR — the pure part, so every
/// state (relay off, draft waiting, route live) is a table a test can read:
/// `&tp=<port>&tt=<token>&tf=<cert sha256>[&th=<host>…][&z=<iroh node>]
///  [&tr=<relay host>&tq=<relay port>][&ta=<digest b64url>]`.
/// `ta` rides only with a draft; a live route has `tr`/`tq` alone.
pub fn pair_fields(
    port: u16,
    token: &str,
    fingerprint: &str,
    hosts: &[String],
    iroh_node: Option<&str>,
    relay: Option<(&str, u16)>,
    pending_digest: Option<&[u8; 32]>,
) -> String {
    let mut ext = format!("&tp={port}&tt={token}&tf={fingerprint}");
    for h in hosts {
        ext.push_str("&th=");
        ext.push_str(&percent_encode(h));
    }
    if let Some(id) = iroh_node {
        ext.push_str("&z=");
        ext.push_str(id);
    }
    if let Some((host, relay_port)) = relay {
        ext.push_str("&tr=");
        ext.push_str(&percent_encode(host));
        ext.push_str(&format!("&tq={relay_port}"));
    }
    if let Some(digest) = pending_digest {
        ext.push_str("&ta=");
        ext.push_str(&b64url(digest));
    }
    ext
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()
}

/// The listener's cert fingerprint is hex on the wire (`tf`); the relay
/// draft wants the same 32 bytes as base64url — Matt's fingerprint shape.
pub fn hex_to_b64url(hex: &str) -> Option<String> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(b64url(&bytes))
}

// ---------------------------------------------------------------- draft life

/// How long an enrollment draft is offered before a fresh one is minted in
/// its place. The relay keeps no state for a draft (it is a route id, a
/// token hash and a digest, all local), so a stale one costs nothing to
/// replace and a phone that signed it a second late simply signs the next.
pub const DRAFT_TTL: Duration = Duration::from_secs(600);
/// How long after the relay server could not be reached before a status
/// read asks it again — the panel polls every 5 s and must not turn a dead
/// relay into a request every 5 s.
pub const DRAFT_RETRY: Duration = Duration::from_secs(30);

/// What the draft slot holds, as a status read sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftSlot {
    /// Nothing prepared, nothing failed.
    Empty,
    /// A draft is waiting for a phone to sign it.
    Waiting { age: Duration },
    /// The last attempt could not reach the relay server.
    Failed { age: Duration },
}

/// What to do with the slot on this read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftStep {
    Keep,
    /// Ask the relay for a route (spawned; the read itself never waits).
    Prepare,
    /// Forget the draft and any error: the road is off or the route is live.
    Drop,
}

/// The eager-draft state machine, pure. A draft exists whenever the relay
/// road is on and no route is enrolled; it is replaced after `DRAFT_TTL`,
/// a failure is retried after `DRAFT_RETRY`, and a prepare already in
/// flight is left alone. Whatever the slot holds, an off road or a live
/// route empties it.
pub fn draft_step(relay_on: bool, route_live: bool, in_flight: bool, slot: DraftSlot) -> DraftStep {
    if !relay_on || route_live {
        return if slot == DraftSlot::Empty { DraftStep::Keep } else { DraftStep::Drop };
    }
    if in_flight {
        return DraftStep::Keep;
    }
    match slot {
        DraftSlot::Empty => DraftStep::Prepare,
        DraftSlot::Waiting { age } if age >= DRAFT_TTL => DraftStep::Prepare,
        DraftSlot::Failed { age } if age >= DRAFT_RETRY => DraftStep::Prepare,
        _ => DraftStep::Keep,
    }
}

// ---------------------------------------------------------------- road order

/// The four roads, in the order a fresh desktop tells phones to try them.
pub const ROADS: [&str; 4] = ["lan", "vpn", "relay", "iroh"];

pub fn default_road_order() -> Vec<String> {
    ROADS.iter().map(|r| r.to_string()).collect()
}

/// A stored order made whole: unknown names dropped, repeats dropped, any
/// road missing appended in default order. The result is always a
/// permutation of `ROADS`.
pub fn normalize_road_order(order: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(ROADS.len());
    for r in order {
        if ROADS.contains(&r.as_str()) && !out.iter().any(|o| o == r) {
            out.push(r.clone());
        }
    }
    for r in ROADS {
        if !out.iter().any(|o| o == r) {
            out.push(r.to_string());
        }
    }
    out
}

/// Exactly the four roads, each once — what `remote_set_road_order` accepts.
pub fn is_road_permutation(order: &[String]) -> bool {
    order.len() == ROADS.len() && normalize_road_order(order) == order
}

// ---------------------------------------------------------------- enrollment

/// The phone's answer to `ta`: its authority key and a signature over the
/// digest. Verified exactly as the gateway verifies its own relay
/// authorization (`remote/auth.rs`) — compressed SEC1 P-256 key, DER ECDSA
/// signature, over the 32-byte draft digest — so the relay never sees a
/// route the phone did not sign for.
pub fn verify_enrollment(digest: &[u8; 32], authority_public_key: &[u8], signature_der: &[u8]) -> Result<(), String> {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    if authority_public_key.len() != 33 {
        return Err("authority key must be a 33-byte compressed P-256 point".into());
    }
    if !(8..=80).contains(&signature_der.len()) {
        return Err("signature must be DER, 8 to 80 bytes".into());
    }
    let authority = VerifyingKey::from_sec1_bytes(authority_public_key).map_err(|_| "authority key is not a P-256 point".to_string())?;
    let signature = Signature::from_der(signature_der).map_err(|_| "signature is not DER ECDSA".to_string())?;
    authority.verify(digest, &signature).map_err(|_| "signature does not verify against the pending digest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse::<Ipv4Addr>().unwrap().into()
    }
    fn v6(s: &str) -> IpAddr {
        s.parse::<Ipv6Addr>().unwrap().into()
    }

    #[test]
    fn cgnat_is_tailscale_whatever_the_interface_is_called() {
        assert_eq!(classify("eth0", v4("100.64.0.1")), Some(Road::Vpn(VpnKind::Tailscale)));
        assert_eq!(classify("eth0", v4("100.127.255.254")), Some(Road::Vpn(VpnKind::Tailscale)));
        assert_eq!(classify("tailscale0", v4("100.101.102.103")), Some(Road::Vpn(VpnKind::Tailscale)));
        // 100.128/9 is ordinary public space, not CGNAT.
        assert_eq!(classify("eth0", v4("100.128.0.1")), Some(Road::Lan));
        assert_eq!(classify("eth0", v4("100.63.255.255")), Some(Road::Lan));
    }

    #[test]
    fn the_interface_name_decides_wireguard_and_utun() {
        assert_eq!(classify("wg0", v4("10.8.0.2")), Some(Road::Vpn(VpnKind::Wireguard)));
        assert_eq!(classify("wg-home", v4("192.168.99.2")), Some(Road::Vpn(VpnKind::Wireguard)));
        assert_eq!(classify("utun3", v4("10.0.0.5")), Some(Road::Vpn(VpnKind::Other)));
        assert_eq!(classify("tailscale0", v6("fd7a:115c:a1e0::1")), Some(Road::Vpn(VpnKind::Tailscale)));
        assert_eq!(classify("wg0", v6("fd00::2")), Some(Road::Vpn(VpnKind::Wireguard)));
    }

    #[test]
    fn ula_is_a_vpn_and_private_v4_is_the_lan() {
        assert_eq!(classify("eth0", v6("fc00::1")), Some(Road::Vpn(VpnKind::Other)));
        assert_eq!(classify("eth0", v6("fdff:ffff::1")), Some(Road::Vpn(VpnKind::Other)));
        assert_eq!(classify("eth0", v6("fe00::1")), Some(Road::Lan)); // just past fc00::/7
        assert_eq!(classify("eth0", v4("192.168.2.176")), Some(Road::Lan));
        assert_eq!(classify("eno1", v4("10.1.2.3")), Some(Road::Lan));
        assert_eq!(classify("eth0", v4("172.16.0.9")), Some(Road::Lan));
    }

    #[test]
    fn unreachable_addresses_belong_to_no_road() {
        assert_eq!(classify("lo", v4("127.0.0.1")), None);
        assert_eq!(classify("lo", v6("::1")), None);
        assert_eq!(classify("eth0", v4("169.254.1.1")), None);
        assert_eq!(classify("eth0", v6("fe80::1")), None);
        assert_eq!(classify("docker0", v4("172.17.0.1")), None);
        assert_eq!(classify("br-1386282af22e", v4("172.27.0.1")), None);
        assert_eq!(classify("virbr0", v4("192.168.122.1")), None);
        assert_eq!(classify("veth1a2b", v4("10.0.0.1")), None);
    }

    fn table() -> Vec<Found> {
        vec![
            ("lo".into(), v4("127.0.0.1")),
            ("eno1".into(), v4("192.168.2.176")),
            ("eno1".into(), v6("fe80::1")),
            ("docker0".into(), v4("172.17.0.1")),
            ("tailscale0".into(), v4("100.101.102.103")),
            ("tailscale0".into(), v6("fd7a:115c:a1e0::1")),
            ("wg0".into(), v4("10.8.0.2")),
            ("eno1".into(), v4("203.0.113.7")),
        ]
    }

    #[test]
    fn advertised_hosts_follow_the_roads_that_are_on() {
        assert_eq!(
            advertised_from(table(), true, true),
            vec!["100.101.102.103", "10.8.0.2", "192.168.2.176", "203.0.113.7"]
        );
        assert_eq!(advertised_from(table(), true, false), vec!["192.168.2.176", "203.0.113.7"]);
        assert_eq!(advertised_from(table(), false, true), vec!["100.101.102.103", "10.8.0.2"]);
        assert!(advertised_from(table(), false, false).is_empty());
    }

    #[test]
    fn vpn_status_prefers_tailscale_and_a_v4_address() {
        let s = vpn_status_from(table());
        assert_eq!(s.kind.as_deref(), Some("tailscale"));
        assert_eq!(s.interface.as_deref(), Some("tailscale0"));
        assert_eq!(s.address.as_deref(), Some("100.101.102.103"));
        let only_wg = vec![("wg0".to_string(), v4("10.8.0.2")), ("eth0".to_string(), v4("192.168.1.2"))];
        let s = vpn_status_from(only_wg);
        assert_eq!(s.kind.as_deref(), Some("wireguard"));
        assert_eq!(vpn_status_from(vec![("eth0".into(), v4("192.168.1.2"))]), VpnStatus::default());
    }

    const TOKEN: &str = "ab";
    const FP: &str = "cd";

    #[test]
    fn pair_fields_with_the_relay_road_off_carry_no_relay_keys() {
        let hosts = vec!["192.168.2.176".to_string(), "100.101.102.103".to_string()];
        let ext = pair_fields(8877, TOKEN, FP, &hosts, Some("node1"), None, None);
        assert_eq!(ext, "&tp=8877&tt=ab&tf=cd&th=192.168.2.176&th=100.101.102.103&z=node1");
        assert!(!ext.contains("&tr=") && !ext.contains("&tq=") && !ext.contains("&ta="));
    }

    #[test]
    fn pair_fields_with_a_draft_carry_the_route_and_the_digest() {
        let digest = [7u8; 32];
        let ext = pair_fields(8877, TOKEN, FP, &[], None, Some(("desktop-1234.relay.example.com", 443)), Some(&digest));
        assert_eq!(
            ext,
            format!("&tp=8877&tt=ab&tf=cd&tr=desktop-1234.relay.example.com&tq=443&ta={}", b64url(&digest))
        );
        assert_eq!(b64url_decode(ext.rsplit("&ta=").next().unwrap()).unwrap(), digest.to_vec());
    }

    #[test]
    fn pair_fields_with_a_live_route_carry_the_route_and_nothing_to_sign() {
        let ext = pair_fields(8877, TOKEN, FP, &[], None, Some(("desktop-1234.relay.example.com", 443)), None);
        assert_eq!(ext, "&tp=8877&tt=ab&tf=cd&tr=desktop-1234.relay.example.com&tq=443");
        assert!(!ext.contains("&ta="));
    }

    #[test]
    fn hosts_ride_percent_encoded_like_the_gateway_list() {
        let ext = pair_fields(1, TOKEN, FP, &["john's box".to_string()], None, Some(("a b.example.com", 8443)), None);
        assert!(ext.contains("&th=john%27s%20box"));
        assert!(ext.contains("&tr=a%20b.example.com&tq=8443"));
    }

    #[test]
    fn the_hex_fingerprint_becomes_matts_base64url_shape() {
        let hex: String = (0u8..32).map(|b| format!("{b:02x}")).collect();
        let b = hex_to_b64url(&hex).unwrap();
        assert_eq!(b64url_decode(&b).unwrap(), (0u8..32).collect::<Vec<_>>());
        assert!(hex_to_b64url("abc").is_none());
        assert!(hex_to_b64url(&"zz".repeat(32)).is_none());
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn an_empty_slot_with_the_road_on_and_no_route_prepares_a_draft() {
        assert_eq!(draft_step(true, false, false, DraftSlot::Empty), DraftStep::Prepare);
    }

    #[test]
    fn a_fresh_draft_is_kept_and_a_stale_one_is_replaced() {
        assert_eq!(draft_step(true, false, false, DraftSlot::Waiting { age: secs(0) }), DraftStep::Keep);
        assert_eq!(draft_step(true, false, false, DraftSlot::Waiting { age: DRAFT_TTL - secs(1) }), DraftStep::Keep);
        assert_eq!(draft_step(true, false, false, DraftSlot::Waiting { age: DRAFT_TTL }), DraftStep::Prepare);
        assert_eq!(draft_step(true, false, false, DraftSlot::Waiting { age: secs(3600) }), DraftStep::Prepare);
    }

    #[test]
    fn a_failure_waits_its_backoff_before_asking_again() {
        assert_eq!(draft_step(true, false, false, DraftSlot::Failed { age: secs(0) }), DraftStep::Keep);
        assert_eq!(draft_step(true, false, false, DraftSlot::Failed { age: DRAFT_RETRY - secs(1) }), DraftStep::Keep);
        assert_eq!(draft_step(true, false, false, DraftSlot::Failed { age: DRAFT_RETRY }), DraftStep::Prepare);
    }

    #[test]
    fn a_prepare_in_flight_is_never_doubled() {
        assert_eq!(draft_step(true, false, true, DraftSlot::Empty), DraftStep::Keep);
        assert_eq!(draft_step(true, false, true, DraftSlot::Waiting { age: secs(9999) }), DraftStep::Keep);
        assert_eq!(draft_step(true, false, true, DraftSlot::Failed { age: secs(9999) }), DraftStep::Keep);
    }

    #[test]
    fn an_off_road_or_a_live_route_empties_the_slot_and_prepares_nothing() {
        for (on, live) in [(false, false), (false, true), (true, true)] {
            assert_eq!(draft_step(on, live, false, DraftSlot::Empty), DraftStep::Keep);
            assert_eq!(draft_step(on, live, false, DraftSlot::Waiting { age: secs(1) }), DraftStep::Drop);
            assert_eq!(draft_step(on, live, false, DraftSlot::Failed { age: secs(1) }), DraftStep::Drop);
            // Even mid-flight: the answer will be discarded when it lands.
            assert_eq!(draft_step(on, live, true, DraftSlot::Waiting { age: secs(1) }), DraftStep::Drop);
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_road_order_is_made_whole_on_load() {
        assert_eq!(normalize_road_order(&[]), strs(&["lan", "vpn", "relay", "iroh"]));
        assert_eq!(normalize_road_order(&strs(&["iroh", "relay"])), strs(&["iroh", "relay", "lan", "vpn"]));
        assert_eq!(normalize_road_order(&strs(&["vpn", "bogus", "vpn", "lan"])), strs(&["vpn", "lan", "relay", "iroh"]));
        assert_eq!(normalize_road_order(&strs(&["relay", "iroh", "vpn", "lan"])), strs(&["relay", "iroh", "vpn", "lan"]));
        assert_eq!(default_road_order(), strs(&["lan", "vpn", "relay", "iroh"]));
    }

    #[test]
    fn only_a_permutation_of_the_four_roads_is_accepted() {
        assert!(is_road_permutation(&strs(&["lan", "vpn", "relay", "iroh"])));
        assert!(is_road_permutation(&strs(&["iroh", "relay", "vpn", "lan"])));
        assert!(!is_road_permutation(&strs(&["lan", "vpn", "relay"])));
        assert!(!is_road_permutation(&strs(&["lan", "vpn", "relay", "iroh", "lan"])));
        assert!(!is_road_permutation(&strs(&["lan", "vpn", "relay", "bogus"])));
        assert!(!is_road_permutation(&strs(&["lan", "lan", "relay", "iroh"])));
        assert!(!is_road_permutation(&[]));
    }

    #[test]
    fn enrollment_verifies_a_real_signature_and_refuses_a_bent_one() {
        use p256::ecdsa::{signature::Signer, Signature, SigningKey};
        use p256::elliptic_curve::rand_core::OsRng;
        let digest = [42u8; 32];
        let authority = SigningKey::random(&mut OsRng);
        let key = authority.verifying_key().to_encoded_point(true);
        let signature: Signature = authority.sign(&digest);
        let der = signature.to_der();
        assert!(verify_enrollment(&digest, key.as_bytes(), der.as_bytes()).is_ok());

        let mut other = digest;
        other[0] ^= 1;
        assert!(verify_enrollment(&other, key.as_bytes(), der.as_bytes()).is_err());

        let mut bent = der.as_bytes().to_vec();
        let last = bent.len() - 1;
        bent[last] ^= 1;
        assert!(verify_enrollment(&digest, key.as_bytes(), &bent).is_err());

        let stranger = SigningKey::random(&mut OsRng);
        let stranger_key = stranger.verifying_key().to_encoded_point(true);
        assert!(verify_enrollment(&digest, stranger_key.as_bytes(), der.as_bytes()).is_err());

        assert!(verify_enrollment(&digest, &key.as_bytes()[..32], der.as_bytes()).is_err());
        assert!(verify_enrollment(&digest, key.as_bytes(), &[0u8; 4]).is_err());
    }
}
