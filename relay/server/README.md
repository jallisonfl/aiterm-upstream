# AITerm Relay

AITerm Relay is a blind transport relay for the existing AITerm remote gateway. It
does not implement sessions, pairing, device trust, terminal rendering, or file
transfer. The Android app still establishes pinned TLS directly with the
desktop and still authenticates with its remembered device key. The relay only
sees encrypted byte counts, timing, a random route id, and network addresses.

Initial connection order is:

1. local LAN address;
2. VPN/overlay address;
3. the route-specific relay hostname.

After a relay connection authenticates, AITerm may use the relay's UDP
rendezvous endpoint to discover the phone and desktop's observed addresses and
upgrade to a direct QUIC tunnel. The established relay remains the fallback if
hole punching or QUIC fails. QUIC carries the same opaque application TLS bytes
as the relay, so this optimization does not introduce a second API or trust
model.

The desktop maintains one outbound WebSocket to the connector listener. A
phone connects to the ingress listener with SNI
`<route-id>.<public-domain>`. The ingress peeks at (but does not consume) that
ClientHello, opens a multiplexed stream to the matching desktop connector, and
then copies the complete TLS stream unchanged.

## Multiple clients and growth

A single relay serves every static or dynamically enrolled route concurrently.
Each desktop/location has a unique route id and connector token, so traffic and
authentication remain isolated. A route accepts one active desktop connector
and multiplexes up to 128 simultaneous phone connections; reconnecting the same
desktop identity replaces its stale connector. The ingress listener bounds the
whole process to roughly 1,024 concurrent phone tasks so an overloaded edge
fails closed instead of consuming memory without limit.

This is intentionally enough for a small shared deployment without changing
the protocol later. When one VM is no longer sufficient, route ids can be
assigned to multiple relay instances by DNS or a TCP edge while the phone and
desktop protocol stays unchanged.

## Build and test

```sh
cargo test --manifest-path relay/Cargo.toml
cargo build --release --manifest-path relay/Cargo.toml
```

The daemon takes one JSON configuration path:

```sh
./relay/server/target/release/aiterm-relay /etc/aiterm-relay/relay.json
```

`relay.example.json` documents the configuration. Connector tokens are random
secrets held only by a desktop; the server stores their lowercase SHA-256
hashes. A route id is public, random, lowercase DNS-label text of 8–63
characters.

## Automatic enrollment during pairing

`GET /v1/info` publishes the relay's non-secret connector URL, public domain,
and ingress port. A desktop uses those values to prepare a route locally when
it creates a pairing QR. The QR binds the proposed relay identity to the
pairing exchange. The phone signs that binding with a dedicated hardware-backed
P-256 authority key, and sends the public key and signature as part of the
normal pairing request.

After the user approves that same pairing request, the desktop submits the
phone's proof to `POST /v1/provision`. The relay verifies the signature and
stores only the route id, connector-token hash, phone-authority fingerprint,
desktop TLS fingerprint, source address, and creation time. The connector token
itself never leaves the desktop. There is no separate grant, relay login, or
second approval prompt.

Dynamic enrollment is bounded by global, per-source-address, and per-authority
route limits, plus a per-source provisioning attempt rate limit. These controls
make this suitable for a small private relay. A public multi-tenant service
would additionally need an account or entitlement layer.

The `routes` array remains available for administratively provisioned static
routes. Generate a static token and its server-side hash with standard tools:

```sh
route="desk-$(openssl rand -hex 12)"
token="$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')"
printf '%s' "$token" | sha256sum
```

The token itself goes into AITerm Settings. Only its hash goes into
`relay.json`.

## Production edge

The intended single-VM layout uses two public ports:

- TCP 80: ACME HTTP challenge for the connector certificate;
- TCP 443: raw phone ingress owned by `aiterm-relay`;
- UDP 443: bounded one-time direct-path rendezvous owned by `aiterm-relay`;
- TCP 8443: ordinary TLS/WebSocket edge, proxied to the connector listener on
  `127.0.0.1:8080`.

This keeps application TLS end-to-end on port 443 while allowing a conventional
ACME proxy to manage the connector certificate on 8443. The desktop setting is
then `wss://control.<domain>:8443/v1/connect`; a route's public endpoint is
`<route-id>.<domain>:443`. DNS needs records for `control.<domain>` and the
wildcard `*.<domain>`.

`deploy/Caddyfile` is the TLS edge template. Its systemd service must receive
`AITERM_RELAY_CONTROL_HOST=control.<domain>`. TLS-ALPN validation is disabled
because port 443 belongs to the opaque phone ingress; Caddy uses the port 80
HTTP challenge instead.

`deploy/aiterm-relay.service` is a hardened service template. Dynamic route
state is written beneath systemd's private `StateDirectory` rather than the
configuration directory.
