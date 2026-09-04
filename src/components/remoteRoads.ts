/**
 * The Remote access panel's road decisions, kept out of the component.
 *
 * A road is one way a phone reaches this desktop (see docs/remote/remote-roads.md).
 * Each card on the panel shows a dot and a sentence; both are decided here
 * from the two listeners' status objects, so the component only draws.
 */
import { listenerLabel, relayLabel, type RemoteStatus } from "../remoteAccess.ts";
import type { PhoneRemoteRoad, PhoneRemoteStatus } from "../ipc";

export type Road = PhoneRemoteRoad;

/** green = live, grey = off, amber = on but something is missing. */
export type Dot = "on" | "off" | "warn";

export interface RoadState {
  on: boolean;
  dot: Dot;
  /** One short line under the road's name. */
  text: string;
  /** Extra lines (the relay road has one per route). */
  lines?: string[];
}

/**
 * Which road an advertised host belongs to. Mirrors the phone's rule in the
 * contract: 100.64/10 and fc00::/7 are VPN carrier space, RFC1918 is the LAN,
 * and anything else routable is treated as VPN because a LAN never hands out
 * a public address.
 */
export function classifyHost(host: string): Road {
  const h = host.replace(/^\[|\]$/g, "").toLowerCase();
  if (h.includes(":")) {
    // fc00::/7 — the first byte is fc or fd.
    return /^f[cd]/.test(h) ? "vpn" : "lan";
  }
  const m = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/.exec(h);
  if (!m) return "lan";
  const a = Number(m[1]);
  const b = Number(m[2]);
  if (a === 100 && b >= 64 && b <= 127) return "vpn";
  if (a === 10) return "lan";
  if (a === 172 && b >= 16 && b <= 31) return "lan";
  if (a === 192 && b === 168) return "lan";
  if (a === 169 && b === 254) return "lan";
  return "vpn";
}

/** Human name for the detected VPN kind. */
export function vpnKindLabel(kind: string | null | undefined): string {
  if (kind === "tailscale") return "Tailscale";
  if (kind === "wireguard") return "WireGuard";
  return "VPN";
}

/** First and last few characters of an iroh node id — enough to recognise,
 *  not enough to fill the row. */
export function shortNodeId(id: string | null | undefined): string {
  if (!id) return "";
  return id.length <= 16 ? id : `${id.slice(0, 8)}…${id.slice(-6)}`;
}

export function lanState(status: RemoteStatus | null, p: PhoneRemoteStatus | null): RoadState {
  const on = !!p?.roads?.lan;
  if (!on) return { on, dot: "off", text: "Off" };
  const parts: string[] = [];
  const hosts = (p?.running ? p.addresses : []).filter((h) => classifyHost(h) === "lan");
  if (hosts.length) parts.push(`Advertising ${hosts.join(", ")}`);
  if (status?.enabled) parts.push(`Gateway on ${listenerLabel(status)}`);
  if (parts.length === 0) {
    return {
      on,
      dot: "warn",
      text: p?.running || status?.enabled
        ? "No LAN address found on this machine."
        : "Nothing is listening. Turn a listener on under Listeners.",
    };
  }
  return { on, dot: "on", text: parts.join(" · ") };
}

export function vpnState(p: PhoneRemoteStatus | null): RoadState {
  const on = !!p?.roads?.vpn;
  const vpn = p?.vpn;
  if (!on) {
    return { on, dot: "off", text: vpn?.detected ? `Off · ${vpnKindLabel(vpn.kind)} found` : "Off" };
  }
  if (!vpn?.detected) {
    return {
      on,
      dot: "warn",
      text: "No VPN interface found. Install Tailscale or bring up WireGuard and this fills in by itself.",
    };
  }
  const bits = [vpnKindLabel(vpn.kind)];
  if (vpn.interface && vpn.kind !== "tailscale") bits.push(vpn.interface);
  if (vpn.address) bits.push(vpn.address);
  if (vpn.magic_dns) bits.push(vpn.magic_dns);
  return { on, dot: p?.running ? "on" : "warn", text: bits.join(" · ") + (p?.running ? "" : " — phone listener is off") };
}

/**
 * The phone listener's relay route in a phrase, in order of precedence:
 * the relay server unreachable; a live route with its connector state; a
 * draft waiting while a phone is connected (it is enrolling now); a draft
 * waiting with no phone on; the road off. A paired phone signs the draft
 * by itself, so no line ever asks for a pairing.
 */
export function phoneRelayLine(p: PhoneRemoteStatus | null): string {
  const r = p?.relay;
  if (!r) return "Phone listener: not available";
  if (!p?.roads?.relay) return "Phone listener: off";
  if (r.error) return `Phone listener: relay unreachable — ${r.error}`;
  if (r.configured) {
    const where = r.host ? ` · ${r.host}${r.port ? `:${r.port}` : ""}` : "";
    if (!p.running) return `Phone listener: route saved, listener off${where}`;
    return `Phone listener: ${r.state}${where}`;
  }
  if (r.pending_enrollment && p.clients.length > 0) {
    return "Phone listener: enrolling with the connected phone…";
  }
  return "Phone listener: route is created when a phone connects — no new pairing needed";
}

/** The gateway's relay route in a phrase, from Matt's relayLabel. */
export function gatewayRelayLine(status: RemoteStatus | null): string {
  if (!status?.relay?.configured) return "Gateway: no route yet — the first approved phone creates it";
  return `Gateway: ${relayLabel(status)}`;
}

export function relayState(status: RemoteStatus | null, p: PhoneRemoteStatus | null): RoadState {
  const on = !!p?.roads?.relay;
  if (!on) return { on, dot: "off", text: "Off" };
  const gatewayLive = !!status?.enabled && status.relay?.state === "connected";
  const phoneLive = !!p?.running && p.relay?.state === "connected";
  const anyRoute = !!status?.relay?.configured || !!p?.relay?.configured;
  // No route anywhere is not a wait on a pairing: the phone listener's
  // route arrives by itself from a connected phone, and the gateway's own
  // line below says what it needs.
  const text = gatewayLive || phoneLive
    ? "Connected"
    : anyRoute ? "Route saved, not connected"
    : p?.relay?.error ? "Relay server unreachable"
    : "No route yet";
  return {
    on,
    dot: gatewayLive || phoneLive ? "on" : "warn",
    text,
    lines: [gatewayRelayLine(status), phoneRelayLine(p)],
  };
}

export function irohState(p: PhoneRemoteStatus | null): RoadState {
  const on = !!p?.roads?.iroh;
  if (!on) return { on, dot: "off", text: "Off" };
  if (!p?.running) return { on, dot: "warn", text: "Phone listener is off" };
  if (!p.iroh_node) return { on, dot: "warn", text: "Starting the iroh node…" };
  return { on, dot: "on", text: `On · node ${shortNodeId(p.iroh_node)}` };
}

/** The four road ids as the panel lists them: the desktop's order, made
 *  whole the way the backend does it — unknown ids dropped, any missing
 *  appended in default order — so a card is never lost to a bad value. */
export const ROAD_IDS: Road[] = ["lan", "vpn", "relay", "iroh"];

export function roadOrderOf(p: PhoneRemoteStatus | null): Road[] {
  const seen: Road[] = [];
  for (const id of p?.road_order ?? []) {
    if ((ROAD_IDS as string[]).includes(id) && !seen.includes(id)) seen.push(id);
  }
  for (const id of ROAD_IDS) if (!seen.includes(id)) seen.push(id);
  return seen;
}

/** The order with `road` moved one step; unchanged at the ends. */
export function movedRoad(order: Road[], road: Road, dir: -1 | 1): Road[] {
  const i = order.indexOf(road);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= order.length) return order;
  const next = order.slice();
  next[i] = order[j];
  next[j] = order[i];
  return next;
}

/** The panel's headline: which roads a phone can actually use right now. */
export function reachSummary(status: RemoteStatus | null, p: PhoneRemoteStatus | null): string {
  const anyListener = !!status?.enabled || !!p?.running;
  if (!anyListener) return "Remote access is off.";
  const live: string[] = [];
  if (lanState(status, p).dot === "on") live.push("LAN");
  const vpn = vpnState(p);
  if (vpn.dot === "on") live.push(vpnKindLabel(p?.vpn?.kind));
  if (relayState(status, p).dot === "on") live.push("AITerm Relay");
  if (irohState(p).dot === "on") live.push("iroh");
  if (live.length === 0) return "Remote access is on, but no road is live yet.";
  return `Phones can reach this desktop by: ${live.join(", ")}`;
}
