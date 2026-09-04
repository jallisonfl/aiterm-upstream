import test from "node:test";
import assert from "node:assert/strict";
import {
  movedRoad,
  phoneRelayLine,
  relayState,
  roadOrderOf,
} from "./components/remoteRoads.ts";
import type { PhoneRemoteStatus } from "./ipc";

function phone(over: Partial<PhoneRemoteStatus> & { relay?: Partial<PhoneRemoteStatus["relay"]> } = {}): PhoneRemoteStatus {
  const { relay, ...rest } = over;
  return {
    enabled: true,
    running: true,
    port: 8877,
    name: "office",
    addresses: [],
    upnp_enabled: false,
    upnp: "off",
    public_address: null,
    fingerprint: null,
    clients: [],
    error: null,
    iroh_enabled: true,
    iroh_node: null,
    roads: { lan: true, vpn: true, relay: true, iroh: true },
    vpn: { detected: false, kind: null, interface: null, address: null, magic_dns: null },
    relay: {
      configured: false,
      state: "off",
      host: null,
      port: null,
      server: "https://relay.example",
      pending_enrollment: false,
      error: null,
      ...relay,
    },
    iroh_relay_url: null,
    road_order: ["lan", "vpn", "relay", "iroh"],
    ...rest,
  };
}

const client = { id: 1, device: "Pixel", os: "Android 16", app: "1.0", address: "10.0.0.2", since: 0 };

test("the relay line says the server is unreachable before anything else", () => {
  const p = phone({ relay: { error: "the relay server could not be reached", pending_enrollment: false }, clients: [client] });
  assert.equal(phoneRelayLine(p), "Phone listener: relay unreachable — the relay server could not be reached");
  assert.equal(relayState(null, p).text, "Relay server unreachable");
});

test("a live route shows its connector state and host", () => {
  const p = phone({ relay: { configured: true, state: "connected", host: "desktop-1.relay.example", port: 443 } });
  assert.equal(phoneRelayLine(p), "Phone listener: connected · desktop-1.relay.example:443");
  assert.equal(relayState(null, p).text, "Connected");
  assert.equal(relayState(null, p).dot, "on");
  const off = phone({ running: false, relay: { configured: true, state: "off", host: "h", port: 443 } });
  assert.equal(phoneRelayLine(off), "Phone listener: route saved, listener off · h:443");
});

test("a waiting draft enrolls with the connected phone, or waits for one", () => {
  const busy = phone({ relay: { pending_enrollment: true }, clients: [client] });
  assert.equal(phoneRelayLine(busy), "Phone listener: enrolling with the connected phone…");
  const idle = phone({ relay: { pending_enrollment: true } });
  assert.equal(phoneRelayLine(idle), "Phone listener: route is created when a phone connects — no new pairing needed");
  // No draft yet (still being prepared) reads the same: nothing to pair for.
  assert.equal(phoneRelayLine(phone()), "Phone listener: route is created when a phone connects — no new pairing needed");
  assert.equal(relayState(null, idle).text, "No route yet");
  assert.ok(!relayState(null, idle).text.includes("pair"));
});

test("the road off is just off", () => {
  const p = phone({ roads: { lan: true, vpn: true, relay: false, iroh: true } });
  assert.equal(phoneRelayLine(p), "Phone listener: off");
  assert.equal(relayState(null, p).text, "Off");
});

test("the card order follows the desktop and is made whole", () => {
  assert.deepEqual(roadOrderOf(null), ["lan", "vpn", "relay", "iroh"]);
  assert.deepEqual(roadOrderOf(phone({ road_order: ["iroh", "relay", "vpn", "lan"] })), ["iroh", "relay", "vpn", "lan"]);
  assert.deepEqual(
    roadOrderOf(phone({ road_order: ["relay", "bogus", "relay"] as unknown as PhoneRemoteStatus["road_order"] })),
    ["relay", "lan", "vpn", "iroh"],
  );
});

test("moving a road swaps it with its neighbour and stops at the ends", () => {
  const order = ["lan", "vpn", "relay", "iroh"] as const;
  assert.deepEqual(movedRoad([...order], "relay", -1), ["lan", "relay", "vpn", "iroh"]);
  assert.deepEqual(movedRoad([...order], "relay", 1), ["lan", "vpn", "iroh", "relay"]);
  assert.deepEqual(movedRoad([...order], "lan", -1), [...order]);
  assert.deepEqual(movedRoad([...order], "iroh", 1), [...order]);
});
