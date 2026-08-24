"""
QoS profiles — the latency/quality tradeoff, as a named choice.

The link constrains *total bytes on the wire*, not video bitrate. Every profile
is one budget split three ways: pixels, redundancy, and headroom. Before this
existed the split was "unbounded pixels, zero redundancy, zero headroom", which
is why motion broke the stream.

**Architectural boundary.** SPEC.md puts encoder settings on the robot's video
publisher and transport policy on DARC, so a profile is not a config object both
sides read — it is a request DARC makes and the publisher answers:

  DARC owns      max_bitrate_kbps, latency_budget_ms, max_gop_ms (targets the
                 transport can observe and enforce), plus its own FEC rates,
                 drop threshold, and the pilot's close-out deadlines.
  Publisher owns resolution, preset, VBV sizing, actual GOP — because only it
                 knows its sensor and its encoder.

That is why there is no resolution column here. DARC never says "use 960x540";
it says "stay under 1500 kbps with a 100 ms latency budget" and the publisher
maps that onto its own capabilities. This is the first concrete piece of
SPEC.md's open question on a codec negotiation API.

Note the deliberate inversion: the *latency* profile carries the *most*
redundancy. It runs a low video rate and spends the headroom on never losing a
frame; `quality` spends it on pixels and accepts occasional loss.
"""

from dataclasses import dataclass, asdict


@dataclass(frozen=True)
class QoSProfile:
    name: str

    # ── targets the publisher must honour ────────────────────────────────────
    max_bitrate_kbps: int
    latency_budget_ms: int      # bounds the publisher's VBV window
    max_gop_ms: int             # longest acceptable gap between recovery points

    # ── DARC's own transport policy ──────────────────────────────────────────
    fec_delta_pct: int          # parity overhead on delta frames
    fec_key_pct: int            # parity overhead on keyframes (higher: bigger
                                # frames, and losing one costs a whole GOP)
    backlog_drop_frames: int    # drop a delta frame if the send queue exceeds
                                # this many frame-times of backlog
    pilot_deadline_delta_ms: int
    pilot_deadline_key_ms: int
    on_loss: str                # 'continue' | 'freeze-until-idr'

    def drop_threshold_bytes(self, fps: int = 30) -> int:
        """
        Backlog above which a delta frame is dropped before any of it is sent.

        Must be a byte budget rather than "queue is non-empty": aioquic's
        pending-datagram list grows both when the congestion window is exhausted
        *and* merely because the pacer is spacing packets out, so a non-empty
        queue does not imply congestion.
        """
        bytes_per_frame = self.max_bitrate_kbps * 1000 / 8 / max(1, fps)
        return int(self.backlog_drop_frames * bytes_per_frame)

    def publisher_config(self) -> dict:
        """The half of the profile the video publisher is responsible for."""
        return {
            'type':             'video-config',
            'profile':          self.name,
            'maxBitrateKbps':   self.max_bitrate_kbps,
            'latencyBudgetMs':  self.latency_budget_ms,
            'maxGopMs':         self.max_gop_ms,
        }

    def pilot_config(self) -> dict:
        """What the pilot needs to know: close-out deadlines and loss policy."""
        return {
            'deadlineDelta': self.pilot_deadline_delta_ms,
            'deadlineKey':   self.pilot_deadline_key_ms,
            'onLoss':        self.on_loss,
        }

    def as_dict(self) -> dict:
        return asdict(self)


PROFILES: dict[str, QoSProfile] = {
    # Total link budget ~1.9 Mbps (1.5 video + ~0.4 parity).
    'latency': QoSProfile(
        name='latency',
        max_bitrate_kbps=1500,
        latency_budget_ms=100,
        max_gop_ms=1000,
        fec_delta_pct=25,
        fec_key_pct=50,
        backlog_drop_frames=1,
        pilot_deadline_delta_ms=20,
        pilot_deadline_key_ms=40,
        on_loss='continue',
    ),
    # Total ~3.5 Mbps.
    'balanced': QoSProfile(
        name='balanced',
        max_bitrate_kbps=3000,
        latency_budget_ms=100,
        max_gop_ms=1000,
        fec_delta_pct=15,
        fec_key_pct=30,
        backlog_drop_frames=2,
        pilot_deadline_delta_ms=30,
        pilot_deadline_key_ms=60,
        on_loss='continue',
    ),
    # Total ~6.6 Mbps.
    'quality': QoSProfile(
        name='quality',
        max_bitrate_kbps=6000,
        latency_budget_ms=200,
        max_gop_ms=2000,
        fec_delta_pct=8,
        fec_key_pct=15,
        backlog_drop_frames=3,
        pilot_deadline_delta_ms=50,
        pilot_deadline_key_ms=100,
        on_loss='freeze-until-idr',
    ),
}

DEFAULT = 'balanced'


def get(name: str | None) -> QoSProfile:
    """Look up a profile, falling back to the default for anything unknown."""
    return PROFILES.get((name or '').strip().lower(), PROFILES[DEFAULT])
