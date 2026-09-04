# Remote roads — one desktop, many ways to reach it

A phone reaches a desktop over a *road*. Roads are independent, any set can be
on at once, and the phone tries them in an order — the desktop's published
one, unless the phone's owner set their own. Every road carries the same
pinned-TLS bytes; none of them can read a session. One pairing is enough for
every road: a phone paired over any of them enrolls the relay route by itself.

| Road | What it is | Desktop side | Phone side |
|---|---|---|---|
| `lan` | Direct, same network | advertise RFC1918 / ULA addresses in the QR | dial `https://<h>:<port>` |
| `vpn` | Direct over Tailscale / WireGuard / any VPN | detect `tailscale0`, `wg*`, `100.64/10`, `fc00::/7`; advertise those addresses; show interface + MagicDNS name when `tailscale` CLI answers | dial the same way; 100.64/10 + wg addresses rank as `vpn` |
| `relay` | AITerm Relay (blind TCP relay, SNI-routed) | a second relay route for the phone listener, enrolled the way the gateway's is | dial `https://<tr>:<tq>` with SNI = `tr`, same cert pin |
| `iroh` | peer-to-peer QUIC, relay fallback, no server of ours | iroh tunnel → phone listener; optional custom relay URL | loopback bridge, unchanged |

Two listeners live on the desktop and both keep working:

- **gateway** (`remote/server.rs`, Matt's) — speaks to the Adroited phone app. Roads: lan, vpn, relay. Untouched by this work.
- **phone listener** (`remote_api.rs`) — speaks to the `mobile/` phone app (com.fivelime.aiterm). Roads: lan, vpn, relay, iroh. This document is about it.

## Desktop config (`~/.local/share/aiterm/remote.json`, struct `Config` in remote_api.rs)

New fields, all `#[serde(default)]`:

```
lan_enabled:   bool   default true
vpn_enabled:   bool   default true
relay_enabled: bool   default false   // a route is enrolled by the first paired phone to read status
iroh_enabled:  bool   default true    // exists today
iroh_relay_url: Option<String>        // None = iroh's default (n0) relays
road_order:    Vec<String> default ["lan","vpn","relay","iroh"]   // always a permutation of the four:
                                      // unknown names dropped on load, missing ones appended in default order
```

The phone-listener relay route persists at `<remote root>/phone-relay.json`
using `remote::relay::RelayConfig` verbatim (same `load`/`save`, same 0600).

### The enrollment draft (`remote_roads::draft_step`, pure)

A draft is in-memory only and *eager*: whenever `relay_enabled` is on and no
`phone-relay.json` exists, one is waiting. It is prepared (`GET <server>/v1/info`
on `remote::DEFAULT_RELAY_SERVER`, `RelayConfig::prepare_enrollment` with the
listener cert's SHA-256 as `desktop_spki_sha256`) at listener start, when the
road is switched on, after `remote_phone_relay_clear`, and lazily on any status
read (`remote_api_status` or `/v1/status`) when the slot needs it. Matt's drafts
have no server-side state, so replacing one is free. The slot is one of:

| slot | on a status read |
|---|---|
| empty | prepare (spawned — a read never waits on the relay server) |
| waiting, younger than 10 min (`DRAFT_TTL`) | keep |
| waiting, 10 min or older | re-prepare; the new draft replaces the old |
| failed, younger than 30 s (`DRAFT_RETRY`) | keep the error |
| failed, 30 s or older | try again |
| any, while a prepare is in flight | keep |
| any, road off or route live | drop the draft and the error |

A failed prepare leaves `pending_enrollment=false` with the message in
`relay.error` (RemoteStatus) / `relay_error` (`/v1/status`). Every prepared,
replaced or consumed draft, and every cleared route, is an `Event::StatusChanged`
to the phones. The QR paths (`remote_pair_payload`, `pair_extension`) await a
needed prepare so the code can carry `ta`; a prepare already in flight is not
waited for — the QR then has no `ta`, and the phone enrolls from status once
paired, which is the ordinary path anyway.

## QR (phone-listener fields, appended to the combined payload by `pair_extension`)

Existing: `&tp=<port>&tt=<token>&tf=<cert sha256 hex>[&z=<iroh node id>]`
New:      `[&tr=<relay public host>&tq=<relay port>][&ta=<digest b64url nopad, 32 bytes>]`

- `tr`/`tq` present when `relay_enabled` and either a route exists or a draft was prepared.
- `ta` present only when a draft is waiting (no route yet). The phone signs it and
  calls the enroll endpoint below, so a first pairing can enroll in one go. `ta`
  absent + `tr` present = route already live. A phone that paired without `ta`
  (or whose enrollment failed) gets the same digest from `/v1/status` later.
- Hosts (`h`) are filtered by `lan_enabled` / `vpn_enabled`: a road that is off
  contributes no addresses. The combined QR still carries the gateway's own `h`
  list untouched; the phone listener's hosts are the ones inside `pair_extension` — add them as
  repeated `th=` so the two lists stay independent. `PairLink` reads `th` when
  present, else falls back to `h`.

## Phone listener HTTP (bearer token, existing router in remote_api.rs)

`GET /v1/status` — existing response gains:
```
"relay": {"host": "<tr>", "port": <tq>} | null      // live route only, never a draft
"relay_enroll": {"digest": "<b64url nopad, 32 bytes>"} | null
                                                    // a draft waiting for any paired phone to sign;
                                                    // null once a route lives, with the road off,
                                                    // or while nothing is waiting
"relay_error": "<why the relay server could not be reached>" | null
                                                    // rides top-level so `relay` keeps the shape
                                                    // older phones parse
"roads": {"lan": bool, "vpn": bool, "relay": bool, "iroh": bool}
"road_order": ["lan","vpn","relay","iroh"]         // the desktop's preference, a permutation of the four
```
Reading status is what makes a draft appear: the read spawns the prepare and
the phone hears `status_changed` when it lands, re-reads, and signs.
`POST /v1/relay/enroll`
```
{"authority_public_key": "<b64url nopad, 33-byte compressed SEC1 P-256>",
 "signature_der": "<b64url nopad DER ECDSA over the 32-byte digest, 8..=80 bytes>"}
→ 200 {"host": "<tr>", "port": <tq>}
→ 409 no pending draft (road off, route already live, or draft never prepared)
→ 400 bad key / signature does not verify against the draft digest — also what a
      phone gets when it signed a digest a re-prepare has since replaced; the
      `status_changed` that announced the replacement makes it read and sign again
→ 502 relay refused (its message passed through)
```
On 200 the desktop: `RelayEnrollmentDraft::register`, save `phone-relay.json`,
start `RelayConnectorHandle::start(config, 127.0.0.1:<phone port>)`, notify
phones (`Event` status change). The digest and signature are exactly Matt's
(`relay-protocol::enrollment_digest`, `RelayConfig::prepare_enrollment` with the
phone listener's cert SHA-256 raw bytes as `desktop_spki_sha256`).

## Tauri commands (remote_api.rs; frontend `phoneRemote*` in ipc.ts)

`remote_api_status` → `RemoteStatus` gains:
```
roads:  {lan, vpn, relay, iroh}: bool
vpn:    {detected: bool, kind: "tailscale"|"wireguard"|"other"|null, interface: string|null,
         address: string|null, magic_dns: string|null}
relay:  {configured: bool, state: "off"|"connecting"|"connected"|"retrying",
         host: string|null, port: number|null, server: string, pending_enrollment: bool,
         error: string|null}          // pending_enrollment: a draft is waiting; error: why none is
iroh_relay_url: string|null
road_order: ["lan","vpn","relay","iroh"]
```
New commands:
```
remote_set_road(road: "lan"|"vpn"|"relay"|"iroh", on: bool)   // remote_set_iroh stays, delegates
remote_set_iroh_relay_url(url: string|null)                    // restarts the tunnel when running
remote_phone_relay_clear()                                     // deprovision + delete phone-relay.json + stop connector;
                                                               // a fresh draft is prepared at once
remote_set_road_order(order: string[])                         // a permutation of the four ids, else Err; StatusChanged
```
`remote_set_road("relay", true)` with no route prepares a draft at once (see
*The enrollment draft* above); the next paired phone to read status enrolls.

The panel (`RemoteSettings.tsx`, decisions in `remoteRoads.ts`) lists the four
road cards in `road_order`, each with ▲/▼ that call `remote_set_road_order`. The
AITerm Relay card's phone-listener line reads, in order of precedence: relay
error → "relay unreachable — <error>"; route live → "<state> · <host:port>";
draft waiting with a phone connected → "enrolling with the connected phone…";
draft waiting, none connected → "route is created when a phone connects — no
new pairing needed"; road off → "off". Only the gateway (Matt's app) needs a
pairing to create its route; its line is his and keeps its wording.

## Phone (`mobile/` app)

- `Desktop` gains `relayHost: String = ""`, `relayPort: Int = 0`, `roadOrder: List<String>`
  default `["lan","vpn","relay","iroh"]` (per desktop, editable in settings), and
  `roadOrderCustom: Boolean = false` — set when the person moves a road on the
  Connection order screen; entries stored before the flag existed read as false.
- Candidate URLs are built per road and tried in `roadOrder`; a road with nothing
  to dial is skipped. Classification of an `h`/`th` host: 100.64/10, fc00::/7,
  and any host that is not RFC1918/link-local → `vpn`; RFC1918 → `lan`.
- Pairing: after the first successful status probe, if the link carried `ta`,
  sign it with a P-256 key in AndroidKeyStore (alias `aiterm-relay-authority-p256-v1`,
  `SHA256withECDSA`, no user-auth requirement) and `POST /v1/relay/enroll`; on
  200 store `relayHost/relayPort`. Failure is non-fatal: the desktop is paired,
  the relay road just stays empty and the status poll fills it in later.
- Every status result — the connect sprint, the `status_changed` handler, the
  drawer's reachability probe (there is no periodic poll) — goes through one
  path (`roadsFrom` + `enrollFromStatus` in `AppViewModel`):
  - `relayHost/relayPort` refresh from `status.relay`; name and iroh node too.
  - `status.relay == null` and `status.relay_enroll.digest` present → sign the
    digest with `RelayAuthority` and `POST /v1/relay/enroll` (`enrollRelay`, the
    same function pairing uses for `ta`); on 200 store the route. At most one
    attempt per digest value per desktop (in memory), so a refused digest is not
    hammered; a fresh draft is a fresh digest. Logged at `Log.i("Aiterm", …)`.
  - `!roadOrderCustom` and `status.road_order` is a complete order (all four,
    each once) different from `roadOrder` → adopt and persist it; a change seen
    in `status_changed` restarts the connect sprint so the new order dials.
- Settings → Connection order: moving a road sets `roadOrderCustom=true`; "Use
  desktop's order" clears it and re-applies the order the desktop last
  published (the next status answer keeps it current).
- Relay dial = plain `https://<relayHost>:<relayPort>` through the existing
  pinned `Api` (OkHttp sets SNI from the URL host; the pin is the same `tf`).
