/**
 * The top of the sidebar: what wants you, and what is working.
 *
 * The list below this is history — every session, by recency or project.
 * That is the wrong first thing to see when four agents are running: the one
 * question on arriving is "is anything blocked on me", and a flat list makes
 * you read every row to answer it. So the live sessions get their own lanes
 * above the list, in the order they matter — blocked first, then busy — and
 * the lanes stay put while the list scrolls under them.
 *
 * Same verdict per session as the home board (`../fleet.ts`): the spine's
 * phase when it has one, the tab's bell and progress otherwise. The two
 * surfaces cannot disagree, and neither can the phone.
 *
 * Working rows are capped: a wall of them would push the list off screen,
 * and past a handful they are status, not a queue. Needs-you rows are never
 * capped — every one of those is something to do.
 */
import { useEffect, useState } from "react";
import { Session, homeAbbrev } from "../ipc";
import type { SpineOverview, TabId } from "../ipc";
import { Alert } from "./AlertBell";
import AgentIcon from "./AgentIcon";
import Icon from "./Icon";
import { agentTint } from "../brand";
import { buildFleet, elapsed, type FleetRow } from "../fleet";
import { Bell, Loader } from "lucide-react";

const WORKING_CAP = 6;

interface Props {
  sessions: Session[];
  overview: Map<string, SpineOverview>;
  liveIds: Set<string>;
  attentionIds: Set<string>;
  busyIds: Set<string>;
  otherAlerts: Alert[];
  /** Slot on screen right now, to mark the row you are already looking at. */
  activeSlot: string | null;
  onSelect: (s: Session) => void;
  onResume: (s: Session) => void;
  onGoTab: (key: TabId) => void;
}

function useSecond(on: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [on]);
  return now;
}

function LaneRow({ row, now, active, onSelect, onResume }: {
  row: FleetRow; now: number; active: boolean;
  onSelect: (s: Session) => void; onResume: (s: Session) => void;
}) {
  const s = row.session;
  const tint = agentTint(s.agent);
  const age = elapsed(row.since, now);
  return (
    <button
      className={"lane-row " + row.phase + (active ? " active" : "")}
      title={`${s.title}\n${s.project_path}${row.detail ? "\n" + row.detail : ""}`}
      onClick={() => (row.live ? onSelect(s) : onResume(s))}
    >
      <span className={"agent-badge" + tint.className} style={tint.style}>
        <AgentIcon agent={s.agent} size={14} />
      </span>
      <span className="lane-text">
        <span className="lane-title">{s.title}</span>
        <span className="lane-detail">
          {row.detail || homeAbbrev(s.group_path || s.project_path)}
        </span>
      </span>
      {age && <span className="lane-age">{age}</span>}
    </button>
  );
}

export default function LiveLanes({
  sessions, overview, liveIds, attentionIds, busyIds, otherAlerts, activeSlot,
  onSelect, onResume, onGoTab,
}: Props) {
  const fleet = buildFleet({ sessions, overview, live: liveIds, attention: attentionIds, busy: busyIds, cap: 0 });
  const now = useSecond(fleet.running.some((r) => r.since !== null));
  const waiting = fleet.needsYou.length + otherAlerts.length;
  const working = fleet.running.slice(0, WORKING_CAP);
  const more = fleet.running.length - working.length;
  if (waiting === 0 && fleet.running.length === 0) return null;

  return (
    <div className="lanes">
      {waiting > 0 && (
        <div className="lane needs">
          <div className="lane-head"><Icon of={Bell} size="sm" /> Needs you <span className="lane-count">{waiting}</span></div>
          {fleet.needsYou.map((r) => (
            <LaneRow key={r.session.id} row={r} now={now} active={activeSlot === r.session.id} onSelect={onSelect} onResume={onResume} />
          ))}
          {otherAlerts.map((a) => (
            <button key={a.key} className="lane-row needs_you" title={a.title} onClick={() => onGoTab(a.key)}>
              <span className="agent-badge"><Icon of={Bell} size="sm" /></span>
              <span className="lane-text">
                <span className="lane-title">{a.title}</span>
                <span className="lane-detail">{a.message ?? "Waiting for your input"}</span>
              </span>
            </button>
          ))}
        </div>
      )}
      {fleet.running.length > 0 && (
        <div className="lane running">
          <div className="lane-head"><Icon of={Loader} size="sm" /> Working <span className="lane-count">{fleet.running.length}</span></div>
          {working.map((r) => (
            <LaneRow key={r.session.id} row={r} now={now} active={activeSlot === r.session.id} onSelect={onSelect} onResume={onResume} />
          ))}
          {more > 0 && <div className="lane-more">+{more} more below</div>}
        </div>
      )}
    </div>
  );
}
