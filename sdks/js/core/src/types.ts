export type ChannelKind = 'video' | 'sensor' | 'command';

export interface ChannelInfo {
  id: number;
  kind: ChannelKind;
  name: string;
  codec: string;      // video: 'avc1.42001f'; sensors/commands: 'json' | 'octet-stream'
  fps?: number;
}

export interface QosInfo {
  profile: string;
  deadline_delta_ms: number;
  deadline_key_ms: number;
  on_loss: 'continue' | 'freeze-until-idr';
}

export interface Candidate {
  url: string;
  label: string;
  priority: number;
  needs_probe: boolean;
  family?: 4 | 6;
}

export type P2pHint = 'likely' | 'lan-only' | 'none';

export interface NatReport {
  ipv4?: { local?: string[]; public?: string; nat?: string; cgnat?: boolean; gateway_external?: string };
  ipv6?: { present?: boolean; global?: string[]; inbound_ok?: boolean | null };
  portmap?: { protocol?: string; external?: string; error?: string; lease_s?: number };
  candidates?: { label: string; ok: boolean | null }[];
  prober?: { reachable?: string[]; unreachable?: string[]; ts?: string };
  hint?: P2pHint;
  [k: string]: unknown;
}

export interface Offer {
  session_id: string;
  robot_id: string;
  role: 'driver' | 'observer';
  candidates: Candidate[];
  cert_fingerprints: string[];
  p2p_hint: P2pHint;
  direction: string;
  nat_report: NatReport | null;
  channels: ChannelInfo[];
}

export type FailureReason =
  | 'no-candidates' | 'all-candidates-timeout' | 'cert-mismatch' | 'token-rejected'
  | 'pilot-udp-blocked' | 'robot-offline' | 'handshake-timeout';

export interface P2pFailure {
  reason: FailureReason;
  natReport: NatReport | null;
  candidates: Candidate[];
  detail?: string;
}

export type SessionState =
  | 'idle' | 'signaling' | 'waiting-robot' | 'connecting' | 'connected' | 'p2p-failed' | 'closed';

export interface LinkQuality {
  state: 'good' | 'degraded' | 'poor' | 'lost';
  rttMs: number | null;
  offsetUs: number | null;
  lossPct: number | null;
  degradedPicture: boolean;
}

export interface AgentStats {
  frames_in?: number; frames_sent?: number; frames_dropped_backlog?: number; frames_skipped_stale?: number;
  keyframes_requested?: number; chunks_sent?: number; parity_sent?: number; bytes_sent?: number;
  rtt_ms?: number; min_rtt_ms?: number; cwnd?: number; delivery_kbps?: number;
  [k: string]: unknown;
}

export interface PilotStats {
  chunksRx: number; bytesRx: number; parityRx: number; chunksDup: number; chunksBadHeader: number;
  chunksDropped: number; chunksMissing: number; chunksTooOld: number;
  framesSeen: number; framesClean: number; framesRecovered: number; framesIncomplete: number;
  /** Chunks that arrived for a frame already decoded/closed (typically parity after data completed). */
  chunksLate: number;
  keyframesClean: number; keyframesLost: number; framesDecoded: number; decodeErrors: number; keyframesRequested: number;
  degraded: boolean;
  kbps: number; kbpsPayload: number; fps: number;
  spreadP50Ms: number; spreadP95Ms: number;
  g2gP50Ms: number | null; g2gP95Ms: number | null;
  rttMs: number | null; offsetUs: number | null;
  decodeQueue: number;
  /** Loss the pilot can see itself (missing chunks of frames it knew about) — biased low. */
  lossEstPct: number;
  /** Loss vs the agent's own send count, over the last ~5 s of agent-stats samples. */
  lossTruePct: number | null;
  pathLabel: string | null;
  // Short aliases (same values) for HUDs and tests.
  path: string | null; lossTrue: number | null; g2gP50: number | null; g2gP95: number | null; rtt: number | null;
  qos: QosInfo | null;
  qosPublisher: string | null;
  agent: AgentStats | null;
  injecting: { rate: number; burst: number } | null;
}

export interface VideoFrameEvent {
  channel: ChannelInfo;
  frame: VideoFrame;
}

export interface SensorEvent {
  channel: ChannelInfo;
  seq: number;
  data: unknown;          // parsed JSON for codec 'json'
  raw: Uint8Array;
  sendTs: number;
}

export interface SessionEvents {
  state: { state: SessionState; detail?: string };
  welcome: { sessionId: string; role: 'driver' | 'observer'; channels: ChannelInfo[]; qos: QosInfo; pathLabel: string };
  frame: VideoFrameEvent;
  sensor: SensorEvent;
  link: LinkQuality;
  stats: PilotStats;
  error: { message: string; fatal: boolean };
  'p2p-failed': P2pFailure;
  'video-size': { width: number; height: number };
}
