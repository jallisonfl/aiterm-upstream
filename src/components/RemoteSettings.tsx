import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";
import { listen } from "@tauri-apps/api/event";
import Row from "./SettingsRow";
import RoadCard, { type RoadOrderControls } from "./RoadCard";
import {
  irohState,
  lanState,
  movedRoad,
  reachSummary,
  relayState,
  roadOrderOf,
  vpnState,
  type Road,
} from "./remoteRoads.ts";
import {
  fingerprintLabel,
  inviteCountdownSeconds,
  inviteToShow,
  lastSeenLabel,
  listenerLabel,
  listenerAddressOptions,
  nextRevokeStep,
  preferredListenerConfig,
  rebindListener,
  relayServerFromConnectorUrl,
  type ListenerConfig,
  type PairingInvite,
  type PendingPairing,
  type RemoteStatus,
  type TrustedDevice,
} from "../remoteAccess.ts";
import {
  remoteApproveDevice,
  remoteBeginPairing,
  remoteBeginPairingCombined,
  remoteDenyDevice,
  remoteDevices,
  remoteInterfaces,
  remotePendingPairings,
  remoteRelayClear,
  remoteRevokeDevice,
  remoteStart,
  remoteStatus,
  remoteStop,
  type PhonePairPayload,
  type PhoneRemoteStatus,
  phoneRemotePairPayload,
  phoneRemoteRelayClear,
  phoneRemoteRotateToken,
  phoneRemoteSetEnabled,
  phoneRemoteSetIrohRelayUrl,
  phoneRemoteSetName,
  phoneRemoteSetPort,
  phoneRemoteSetRoad,
  phoneRemoteSetRoadOrder,
  phoneRemoteSetUpnp,
  phoneRemoteStatus,
} from "../ipc";

const DEFAULT_PORT = 8443;
const LISTENER_PREFERENCE_KEY = "aiterm.remote.listener";
/** Matches `DEFAULT_RELAY_SERVER` in `src-tauri/src/remote/mod.rs`. Shown
 *  only when neither listener has told us which server it enrolled with. */
const DEFAULT_RELAY_SERVER = "https://control.34-23-107-73.sslip.io:8443";

function loadListenerPreference(): ListenerConfig | null {
  try {
    const value = JSON.parse(localStorage.getItem(LISTENER_PREFERENCE_KEY) ?? "null");
    if (
      typeof value?.address === "string" &&
      Number.isInteger(value?.port) &&
      value.port >= 1024 &&
      value.port <= 65535
    ) {
      return value;
    }
  } catch { /* A corrupt renderer preference falls back to live discovery. */ }
  return null;
}

function saveListenerPreference(config: ListenerConfig) {
  try {
    localStorage.setItem(LISTENER_PREFERENCE_KEY, JSON.stringify(config));
  } catch { /* Private mode only makes the selection session-local. */ }
}

/**
 * Settings → Remote access: every way a phone reaches this desktop, in one
 * panel, read top to bottom.
 *
 * At the top: one sentence saying which roads are live, and one "Pair phone"
 * button. Then the roads themselves — LAN, VPN, AITerm Relay, iroh — each a
 * card with a switch, a dot, and its own settings behind a disclosure. Any
 * set of roads can be on at once (docs/remote/remote-roads.md). Under "Listeners",
 * collapsed, the two sockets that actually answer: the gateway (the AITerm
 * phone app, per-device trust) and the phone listener (the phone listener, token
 * trust). Either, or both, can be on.
 *
 * Pairing is one button. With both listeners on it mints a combined QR —
 * the gateway's single-use invite with the phone listener's fields riding
 * behind under their own names — that either phone app can scan; with one
 * on, it shows that listener's own code. Every QR is rendered by the
 * backend: no pairing secret ever exists as a string in this webview.
 *
 * Trust decisions stay where they were: gateway devices are approved and
 * revoked here and nowhere else, and the phone listener's token is rotated
 * here ("forget every phone").
 */
export default function RemoteSettings() {
  // ---- gateway state
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [addresses, setAddresses] = useState<string[]>([]);
  const [address, setAddress] = useState<string>("");
  const [port, setPort] = useState(DEFAULT_PORT);
  const [invite, setInvite] = useState<PairingInvite | null>(null);
  const [pending, setPending] = useState<PendingPairing[]>([]);
  const [devices, setDevices] = useState<TrustedDevice[]>([]);
  /** Device the revoke button is armed for; a second click on the same row commits. */
  const [armed, setArmed] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  // ---- phone listener state
  const [pstatus, setPstatus] = useState<PhoneRemoteStatus | null>(null);
  const [pair, setPair] = useState<PhonePairPayload | null>(null);
  const [pairError, setPairError] = useState<string | null>(null);
  const [pname, setPname] = useState("");
  const [pport, setPport] = useState("");
  const [pportError, setPportError] = useState<string | null>(null);
  const [irohRelay, setIrohRelay] = useState("");
  const [confirmForget, setConfirmForget] = useState(false);
  const [confirmRelay, setConfirmRelay] = useState<"gateway" | "phone" | null>(null);
  // The 5s poll must not type over the person: while a field has focus,
  // its value belongs to them, not to the status refresh.
  const editing = useRef(false);

  const refresh = useCallback(async () => {
    const nextStatus = await remoteStatus();
    setStatus(nextStatus);
    remoteDevices().then(setDevices).catch(() => setDevices([]));
    remotePendingPairings().then(setPending).catch(() => setPending([]));
    return nextStatus;
  }, []);

  const loadPhone = useCallback(() => phoneRemoteStatus().then((s) => {
    setPstatus(s);
    if (!editing.current) {
      setPname(s.name);
      setPport(String(s.port));
      setIrohRelay(s.iroh_relay_url ?? "");
    }
  }), []);

  useEffect(() => {
    Promise.all([refresh(), remoteInterfaces()])
      .then(([currentStatus, found]) => {
        setAddresses(found);
        const initial = preferredListenerConfig(
          currentStatus,
          found,
          loadListenerPreference(),
        );
        setAddress(initial.address);
        setPort(initial.port);
      })
      .catch(() => {
        setStatus(null);
        setAddresses([]);
      });
    loadPhone().catch(() => setPstatus(null));
  }, [refresh, loadPhone]);

  // Phone connections come and go on their own schedule. The same tick keeps
  // the gateway's relay line honest — its connector state moves without any
  // click on this side.
  useEffect(() => {
    const un = listen("remote://clients", () => loadPhone().catch(() => {}));
    const t = setInterval(() => {
      setNow(Date.now());
      loadPhone().catch(() => {});
      remoteStatus().then(setStatus).catch(() => {});
    }, 5000);
    return () => { un.then((f) => f()); clearInterval(t); };
  }, [loadPhone]);

  // A phone that scans the QR appears here only once the desktop notices it,
  // so poll while a pairing is actually in flight — and only then.
  useEffect(() => {
    if (!invite) return;
    const timer = setInterval(() => {
      setNow(Date.now());
      remotePendingPairings().then(setPending).catch(() => {});
    }, 1000);
    return () => clearInterval(timer);
  }, [invite]);

  const shownInvite = status ? inviteToShow(status, invite, now) : null;
  const addressOptions = listenerAddressOptions(address, addresses);
  // Drop a spent invite from state as well as from the screen, so the next
  // "Pair phone" starts clean rather than flashing the dead one.
  useEffect(() => {
    if (invite && !shownInvite) setInvite(null);
  }, [invite, shownInvite]);

  const run = (work: Promise<unknown>, onSuccess?: () => void) => {
    setError(null);
    work
      .then(() => onSuccess?.())
      .catch((cause) => setError(String(cause)))
      .finally(() => { refresh().catch(() => setStatus(null)); });
  };
  /** Phone-listener writes: the reply is the new status; a failure is shown, not thrown. */
  const runPhone = (work: Promise<PhoneRemoteStatus>, andThen?: () => void) => {
    setError(null);
    work
      .then((s) => { setPstatus(s); andThen?.(); })
      .catch((cause) => setError(String(cause)));
  };

  const gatewayOn = !!status?.enabled;
  const phoneOn = !!pstatus?.enabled;

  const togglePhone = (on: boolean) =>
    runPhone(phoneRemoteSetEnabled(on), () => { if (!on) setPair(null); });
  // A road change alters what the next QR advertises, so a code already on
  // screen would pair the phone for yesterday's roads. Take it down.
  const setRoad = (road: Road, on: boolean) =>
    runPhone(phoneRemoteSetRoad(road, on), () => { setPair(null); setInvite(null); });
  // One button. Both listeners on → the combined QR either app scans;
  // one on → that listener's own code.
  const showQr = async () => {
    setPairError(null);
    setError(null);
    setNow(Date.now());
    try {
      if (gatewayOn && phoneOn) {
        setPair(null);
        setInvite(await remoteBeginPairingCombined());
      } else if (gatewayOn) {
        setPair(null);
        setInvite(await remoteBeginPairing());
      } else if (phoneOn) {
        setInvite(null);
        setPair(await phoneRemotePairPayload());
      } else {
        setPairError("Turn a listener on first (under Listeners, below).");
      }
    } catch (e) { setPair(null); setPairError(`${e}`); }
    // A pairing may have just prepared a relay draft; show it.
    loadPhone().catch(() => {});
  };
  const forget = () =>
    runPhone(phoneRemoteRotateToken(), () => { setPair(null); setConfirmForget(false); });
  const commitName = async () => {
    if (!pstatus || pname.trim() === pstatus.name) return;
    runPhone(phoneRemoteSetName(pname), () => setPair(null));
  };
  const commitPort = async () => {
    if (!pstatus) return;
    const p = Number(pport);
    if (!Number.isInteger(p) || p < 1024 || p > 65535) { setPportError("Pick a port from 1024 to 65535"); return; }
    if (p === pstatus.port) { setPportError(null); return; }
    try { setPstatus(await phoneRemoteSetPort(p)); setPportError(null); setPair(null); }
    catch (e) { setPportError(`${e}`); }
  };
  const commitIrohRelay = () => {
    if (!pstatus) return;
    const next = irohRelay.trim();
    if (next === (pstatus.iroh_relay_url ?? "")) return;
    runPhone(phoneRemoteSetIrohRelayUrl(next || null));
  };
  const blurOnEnter = (e: React.KeyboardEvent<HTMLInputElement>) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); };

  if (!status && !pstatus) {
    return <div className="sgroup-foot">Looking…</div>;
  }

  const lan = lanState(status, pstatus);
  const vpn = vpnState(pstatus);
  const relay = relayState(status, pstatus);
  const iroh = irohState(pstatus);
  const relayServer =
    relayServerFromConnectorUrl(status?.relay?.connector_url)
    ?? pstatus?.relay?.server
    ?? DEFAULT_RELAY_SERVER;

  const qrAudience = gatewayOn && phoneOn
    ? "Either phone app can scan this — each reads its own half."
    : gatewayOn
      ? "For a phone that pairs through the gateway."
      : "For a phone that pairs through the phone listener.";

  // The cards in the order phones try them. ▲/▼ swap neighbours; the
  // desktop publishes the new order and phones without their own pick it up.
  const order = roadOrderOf(pstatus);
  const moveRoad = (road: Road, dir: -1 | 1) => {
    const next = movedRoad(order, road, dir);
    if (next !== order) runPhone(phoneRemoteSetRoadOrder(next));
  };
  const orderOf = (road: Road): RoadOrderControls => ({
    up: order.indexOf(road) > 0,
    down: order.indexOf(road) < order.length - 1,
    onUp: () => moveRoad(road, -1),
    onDown: () => moveRoad(road, 1),
  });
  const cards: Record<Road, ReactElement> = {
    lan: (
      <RoadCard
        key="lan"
        order={orderOf("lan")}
        name="Direct (LAN)"
        desc="Same Wi-Fi or wired network. No servers involved."
        state={lan}
        disabled={!pstatus}
        onToggle={(on) => setRoad("lan", on)}
      >
        {status && (
          <Row
            label="Gateway address"
            desc="Choose a LAN or VPN address to bind. Applying a live change briefly reconnects phones."
          >
            <div className="remote-listener-control">
              <select
                className="set-select mono"
                value={address}
                onChange={(e) => {
                  const next = e.target.value;
                  setAddress(next);
                  if (!status.enabled) saveListenerPreference({ address: next, port });
                }}
              >
                {addressOptions.length === 0 && <option value="">No LAN or VPN address</option>}
                {addressOptions.map((candidate) => (
                  <option key={candidate} value={candidate}>{candidate}</option>
                ))}
              </select>
              {status.enabled && status.address && status.port !== null &&
                (address !== status.address || port !== status.port) && (
                  <button
                    className="set-recheck"
                    disabled={!address}
                    onClick={() => {
                      const current = { address: status.address!, port: status.port! };
                      const target = { address, port };
                      setError(null);
                      rebindListener(
                        current,
                        target,
                        remoteStop,
                        (config) => remoteStart(config.address, config.port),
                      )
                        .then(() => saveListenerPreference(target))
                        .catch((cause) => {
                          setAddress(current.address);
                          setPort(current.port);
                          saveListenerPreference(current);
                          setError(String(cause));
                        })
                        .finally(() => { refresh().catch(() => setStatus(null)); });
                    }}
                  >Apply</button>
                )}
            </div>
          </Row>
        )}
        {status && (
          <Row label="Gateway port">
            <input
              className="set-input"
              type="number"
              min={1024}
              max={65535}
              value={port}
              onChange={(e) => {
                const next = Number(e.target.value) || DEFAULT_PORT;
                setPort(next);
                if (!status.enabled) saveListenerPreference({ address, port: next });
              }}
            />
          </Row>
        )}
        {pstatus && (
          <Row
            label="Phone listener port"
            desc={pportError ?? "Changing it moves the listener and the router mapping at once. Phones must scan a new QR afterwards — the port is in it."}
          >
            <input
              type="number" min={1024} max={65535} value={pport}
              onChange={(e) => setPport(e.target.value)}
              onFocus={() => { editing.current = true; }}
              onBlur={() => { editing.current = false; commitPort(); }} onKeyDown={blurOnEnter}
              style={{ width: 90 }}
            />
          </Row>
        )}
        {pstatus && (
          <Row
            label="Advertised addresses"
            desc="Every address the QR carries for the phone listener, best first. A road that is off contributes none."
            wide
          >
            {pstatus.addresses.length === 0
              ? <span className="diag-val">{pstatus.running ? "none found" : "listener off"}</span>
              : <code className="diag-val">{pstatus.addresses.join("  ")}</code>}
          </Row>
        )}
      </RoadCard>
    ),
    vpn: (
      <RoadCard
        key="vpn"
        order={orderOf("vpn")}
        name="VPN (Tailscale / WireGuard)"
        desc="Your own private network. Works anywhere both devices are on the VPN."
        state={vpn}
        disabled={!pstatus}
        onToggle={(on) => setRoad("vpn", on)}
      >
        <div className="sgroup-foot road-note">
          Nothing to set. The VPN address goes into the QR alongside the LAN
          ones, and the phone prefers it whenever it is not on this network.
          {pstatus?.vpn?.interface ? ` Interface: ${pstatus.vpn.interface}.` : ""}
        </div>
      </RoadCard>
    ),
    relay: (
      <RoadCard
        key="relay"
        order={orderOf("relay")}
        name="AITerm Relay"
        desc="A relay run by AITerm forwards encrypted bytes when there is no direct path. It cannot read sessions."
        state={relay}
        disabled={!pstatus}
        onToggle={(on) => setRoad("relay", on)}
      >
        <Row label="Relay server" desc="Set by AITerm. Both apps enroll here." wide>
          <code className="diag-val">{relayServer}</code>
        </Row>
        {status?.relay?.configured && (
          <Row
            label="Gateway route"
            desc={gatewayOn ? "Turn the gateway off under Listeners to remove its route." : "Deprovisions the route on the relay and forgets it here."}
          >
            {confirmRelay === "gateway" ? (
              <span style={{ display: "inline-flex", gap: 8 }}>
                <button className="act-btn danger" onClick={() => { setConfirmRelay(null); run(remoteRelayClear()); }}>Remove</button>
                <button className="act-btn" onClick={() => setConfirmRelay(null)}>Keep</button>
              </span>
            ) : (
              <button className="set-recheck" disabled={gatewayOn} onClick={() => setConfirmRelay("gateway")}>Remove relay</button>
            )}
          </Row>
        )}
        {pstatus?.relay?.configured && (
          <Row
            label="Phone listener route"
            desc="Deprovisions the route on the relay and forgets it here. The next phone to connect makes a new one."
          >
            {confirmRelay === "phone" ? (
              <span style={{ display: "inline-flex", gap: 8 }}>
                <button className="act-btn danger" onClick={() => { setConfirmRelay(null); runPhone(phoneRemoteRelayClear(), () => setPair(null)); }}>Remove</button>
                <button className="act-btn" onClick={() => setConfirmRelay(null)}>Keep</button>
              </span>
            ) : (
              <button className="set-recheck" onClick={() => setConfirmRelay("phone")}>Remove relay</button>
            )}
          </Row>
        )}
        <div className="sgroup-foot road-note">
          A paired phone enrolls the phone listener's route by itself the next
          time it connects while this is on — no new pairing. Only the gateway
          (the AITerm phone app) needs a pairing to create its route.
        </div>
      </RoadCard>
    ),
    iroh: (
      <RoadCard
        key="iroh"
        order={orderOf("iroh")}
        name="iroh (peer-to-peer)"
        desc="Direct peer-to-peer when it can, public iroh relays when it can't. Nothing of ours in the middle. Phone listener only."
        state={iroh}
        disabled={!pstatus}
        onToggle={(on) => setRoad("iroh", on)}
      >
        {pstatus?.iroh_node && (
          <Row label="Node id" desc="What the phone dials. It is in the QR." wide>
            <code className="diag-val" style={{ fontSize: 11 }}>{pstatus.iroh_node}</code>
          </Row>
        )}
        <Row
          label="Relay server"
          desc="Run your own iroh relay for a fully private path; leave empty for the public ones."
        >
          <input
            type="text"
            value={irohRelay}
            placeholder="iroh default (n0)"
            onChange={(e) => setIrohRelay(e.target.value)}
            onFocus={() => { editing.current = true; }}
            onBlur={() => { editing.current = false; commitIrohRelay(); }}
            onKeyDown={blurOnEnter}
            style={{ width: 220 }}
          />
        </Row>
      </RoadCard>
    ),
  };

  return (
    <>
      <div className="sgroup">
        <div className="sgroup-title">Remote access</div>
        <div className="sgroup-rows">
          <Row
            label={reachSummary(status, pstatus)}
            desc={gatewayOn
              ? "The code is single use, and it stops working after five minutes."
              : "Open the app on the phone, tap Scan, and scan."}
          >
            <button
              className="set-recheck"
              disabled={!gatewayOn && !phoneOn}
              onClick={showQr}
            >Pair phone</button>
          </Row>
          {shownInvite && (
            <Row label="Scan this on your phone" wide>
              <div className="remote-qr">
                {/* The backend rendered this; the payload it encodes never
                    became a string in the renderer. */}
                <div dangerouslySetInnerHTML={{ __html: shownInvite.svg }} />
                <div className="sgroup-foot">
                  {qrAudience} Expires in {inviteCountdownSeconds(shownInvite, now)}s
                </div>
              </div>
            </Row>
          )}
          {!shownInvite && pair && (
            <Row label="Scan this on your phone" wide>
              <div className="remote-qr" dangerouslySetInnerHTML={{ __html: pair.svg }} />
              <div className="sgroup-foot">{qrAudience}</div>
            </Row>
          )}
          {pairError && <div className="set-notice">{pairError}</div>}
          {pending.length > 0 && (
            <div className="agent-list">
              {pending.map((request) => (
                <div key={request.id} className="agent-row">
                  <div className="agent-text">
                    <div className="agent-name">{request.name} wants to pair</div>
                    <div className="srow-desc">
                      Key {fingerprintLabel(request.fingerprint)}
                    </div>
                  </div>
                  <button
                    className="set-recheck"
                    onClick={() => run(remoteApproveDevice(request.id).then(() => setInvite(null)))}
                  >Approve</button>
                  <button
                    className="set-recheck"
                    onClick={() => run(remoteDenyDevice(request.id))}
                  >Deny</button>
                </div>
              ))}
            </div>
          )}
          <div className="sgroup-foot">
            One QR carries both the gateway and the phone listener — each phone app
            reads its own half. Every road below that is on rides in the code.
          </div>
        </div>
      </div>

      <div className="sgroup">
        <div className="sgroup-title">Ways to reach this desktop</div>
        <div className="sgroup-foot road-order-note">
          Phones try roads in this order; a phone can set its own order in its settings.
        </div>
        <div className="road-list">
          {order.map((road) => cards[road])}
        </div>
        {pstatus?.error && <div className="set-notice">{pstatus.error}</div>}
      </div>

      <div className="sgroup">
        <details className="road-listeners">
          <summary className="sgroup-title">Listeners</summary>
          <div className="sgroup-rows">
            <Row
              label="Gateway"
              desc={status
                ? gatewayOn
                  ? `On · ${listenerLabel(status)} · per-device approval. Fingerprint ${fingerprintLabel(status.fingerprint ?? "")}`
                  : "Off. The structured-API listener; phones are approved one by one."
                : "Not available."}
            >
              <label className="sw" aria-label="Gateway">
                <input
                  type="checkbox"
                  checked={gatewayOn}
                  disabled={!status || (!gatewayOn && !address)}
                  onChange={() =>
                    run(
                      gatewayOn ? remoteStop() : remoteStart(address, port),
                      () => saveListenerPreference({ address, port }),
                    )
                  }
                />
                <span className="sw-track"><span className="sw-knob" /></span>
              </label>
            </Row>
            <Row
              label="Phone listener"
              desc={pstatus
                ? pstatus.running
                  ? `On · port ${pstatus.port} · one shared secret. ${pstatus.fingerprint ? `Fingerprint ${fingerprintLabel(pstatus.fingerprint)}` : ""}`
                  : "Off. The token-paired listener; nothing listens until you turn it on."
                : "Not available."}
            >
              <label className="sw" aria-label="Phone listener">
                <input type="checkbox" checked={phoneOn} disabled={!pstatus} onChange={(e) => togglePhone(e.target.checked)} />
                <span className="sw-track"><span className="sw-knob" /></span>
              </label>
            </Row>
            {pstatus && (
              <Row label="This machine's name" desc="What a paired phone shows for this desktop">
                <input
                  type="text" value={pname}
                  onChange={(e) => setPname(e.target.value)}
                  onFocus={() => { editing.current = true; }}
                  onBlur={() => { editing.current = false; commitName(); }} onKeyDown={blurOnEnter}
                  style={{ width: 180 }}
                />
              </Row>
            )}
            {pstatus && (
              <Row label="Router port forward (UPnP)" desc={
                !pstatus.upnp_enabled
                  ? "Off. This machine never asks the router to open a port — not for the listener, not through iroh. Phones off this network use a road above."
                  : !pstatus.running ? "On. When the listener starts, the router is asked to forward its port."
                  : pstatus.upnp === "mapped" && pstatus.public_address
                  ? `The router is forwarding port ${pstatus.port}. The phone reaches this machine at ${pstatus.public_address} from any network.`
                  : pstatus.upnp === "searching" ? "Asking the router to forward the port…"
                  : pstatus.upnp === "no_router" ? "No UPnP router answered. From outside this network the phone needs a road above."
                  : pstatus.upnp === "refused" ? "The router refused to forward the port. Use a road above, or turn UPnP on in the router."
                  : "On"
              }>
                <label className="sw" aria-label="Router port forward">
                  <input
                    type="checkbox"
                    checked={pstatus.upnp_enabled}
                    onChange={(e) => runPhone(phoneRemoteSetUpnp(e.target.checked), () => { setPair(null); setInvite(null); })}
                  />
                  <span className="sw-track"><span className="sw-knob" /></span>
                </label>
              </Row>
            )}
            {pstatus && (
              <Row label="Forget every phone" desc="Rotates the phone listener secret. Each phone must scan a new QR to reconnect.">
                {confirmForget ? (
                  <span style={{ display: "inline-flex", gap: 8 }}>
                    <button className="act-btn danger" onClick={forget}>Forget</button>
                    <button className="act-btn" onClick={() => setConfirmForget(false)}>Keep</button>
                  </span>
                ) : (
                  <button className="act-btn" onClick={() => setConfirmForget(true)}>Forget…</button>
                )}
              </Row>
            )}
          </div>
        </details>
      </div>

      {pstatus && (
        <div className="sgroup">
          <div className="sgroup-title">Connected now</div>
          <div className="sgroup-rows">
            <Row label="Phone listener" desc={pstatus.clients.length === 0
              ? (pstatus.running ? "No phone is connected." : "Listener off.")
              : `${pstatus.clients.length} ${pstatus.clients.length === 1 ? "phone" : "phones"} holding a live connection.`} wide={pstatus.clients.length > 0}>
              {pstatus.clients.length > 0 && (
                <div className="remote-clients">
                  {pstatus.clients.map((c) => (
                    <div key={c.id} className="remote-client">
                      <strong>{c.device || "Unknown device"}</strong>
                      <span className="dim"> · {c.os || "?"} · app {c.app || "?"} · from {c.address} · {sinceLabel(c.since, now)}</span>
                    </div>
                  ))}
                </div>
              )}
            </Row>
          </div>
        </div>
      )}

      {status && (
        <div className="sgroup">
          <div className="sgroup-title">Paired phones (gateway)</div>
          <div className="sgroup-rows">
            {devices.length === 0 ? (
              <div className="sgroup-foot">No phones paired.</div>
            ) : (
              <div className="agent-list">
                {devices.map((device) => (
                  <div key={device.id} className="agent-row">
                    <div className="agent-text">
                      <div className="agent-name">{device.name}</div>
                      <div className="srow-desc">
                        {lastSeenLabel(device, now)} — IP {device.last_ip ?? "not recorded"}
                      </div>
                      <div className="srow-desc">
                        Key {fingerprintLabel(device.fingerprint)}
                      </div>
                    </div>
                    <button
                      className="set-recheck"
                      onClick={() => {
                        const step = nextRevokeStep(armed, device.id);
                        if (step === "confirmed") {
                          setArmed(null);
                          run(remoteRevokeDevice(device.id));
                        } else {
                          setArmed(step);
                        }
                      }}
                      onBlur={() => setArmed((current) => (current === device.id ? null : current))}
                    >
                      {armed === device.id ? "Confirm revoke" : "Revoke"}
                    </button>
                  </div>
                ))}
              </div>
            )}
            <div className="sgroup-foot">
              Revoking forgets the phone's key and drops its connection. Turning
              the gateway off does not — the phone stays trusted for next time.
            </div>
          </div>
        </div>
      )}

      {error && <div className="set-notice">{error}</div>}
    </>
  );
}

function sinceLabel(since: number, now: number): string {
  const s = Math.max(0, Math.floor(now / 1000 - since));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}
