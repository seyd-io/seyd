# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Annex B H.264 → RTP (RFC 6184) for seydd's `rtp://` input.

The drone emits an elementary stream, not RTP, so something has to add the
framing seydd's depacketizer reads: one timestamp per picture on the 90 kHz
clock, the marker bit on a picture's last packet, single-NAL packets for
small NAL units and FU-A fragmentation for large ones. This is what FFmpeg's
`-f rtp` muxer does for the webcam simulation; here it is fifty lines of
stdlib because the demo host should not need FFmpeg between the drone and
the daemon.

It also keeps two rules the daemon and the browser depend on:

* **Parameter sets travel with the keyframe.** The drone sends SPS/PPS as a
  picture of their own when asked to start video; seydd would emit that as an
  empty non-keyframe and the IDR after it without its SPS, which a fresh
  decoder cannot start from. So SPS and PPS are cached and re-sent in front of
  every IDR in the same access unit, and a picture without a slice is not sent.
* **A torn picture is never forwarded, and a keyframe is asked for at once.**
  What happens to the delta frames that follow, until that keyframe arrives,
  is `after_loss`:
  - `forward` (the default): they keep flowing. They reference a picture the
    decoder never got, so it conceals — a smear for the few frames the
    keyframe takes to arrive — but motion continues. This is what the
    drone's own app does, and it is what a pilot wants: measured on the
    first flights, the drone's 2.4 GHz link tore a picture every few seconds
    and in bursts twice a second, and freezing for each one read as constant
    stutter while the same losses were barely visible in the vendor app.
  - `wait`: they are dropped until the keyframe, bounded by `resume_after_s`
    — a short freeze instead of a smear, for a decoder that cannot tolerate
    a missing reference.
  Either way the request is repeated every 500 ms until an IDR is seen, and
  nothing at all is forwarded before the stream's first decodable keyframe.
"""
from __future__ import annotations

import logging
import os
import struct
from typing import Callable, Iterable, List, Optional, Tuple

log = logging.getLogger('h264rtp')

RTP_CLOCK_HZ = 90_000
PAYLOAD_TYPE = 96
MTU_PAYLOAD = 1400   # under a 1500-byte MTU with IPv4/UDP/RTP headers, and loopback anyway

NAL_NON_IDR = 1
NAL_IDR = 5
NAL_SEI = 6
NAL_SPS = 7
NAL_PPS = 8
NAL_AUD = 9


def split_annexb(buf: bytes) -> List[bytes]:
    """NAL units without start codes, in order. Accepts 3- and 4-byte start codes and a stream with neither."""
    nals: List[bytes] = []
    n = len(buf)
    i = buf.find(b'\x00\x00\x01')
    if i < 0:
        return [buf] if buf else []
    start = i + 3
    while True:
        j = buf.find(b'\x00\x00\x01', start)
        if j < 0:
            nal = buf[start:n]
            if nal:
                nals.append(nal)
            return nals
        end = j
        # a 4-byte start code is 00 00 00 01: the extra zero belongs to the code, not the NAL
        if end > start and buf[end - 1] == 0:
            end -= 1
        nal = buf[start:end]
        if nal:
            nals.append(nal)
        start = j + 3


def sps_codec_string(sps: bytes) -> str:
    """The WebCodecs identifier the stream really is: `avc1.PPCCLL` from profile_idc, constraint flags, level_idc."""
    if len(sps) < 4:
        return '?'
    return f'avc1.{sps[1]:02x}{sps[2]:02x}{sps[3]:02x}'


class Packetizer:
    """Stateless RTP packetization with the sequence/SSRC state of one stream."""

    def __init__(self, ssrc: Optional[int] = None):
        self.seq = int.from_bytes(os.urandom(2), 'big')
        self.ssrc = ssrc if ssrc is not None else int.from_bytes(os.urandom(4), 'big')

    def _hdr(self, marker: bool, ts: int) -> bytes:
        h = struct.pack('!BBHII', 0x80, (0x80 if marker else 0) | PAYLOAD_TYPE, self.seq, ts & 0xffffffff, self.ssrc)
        self.seq = (self.seq + 1) & 0xffff
        return h

    def packets(self, nals: Iterable[bytes], ts: int) -> List[bytes]:
        """All packets of one access unit; the marker sits on the last one."""
        nals = [n for n in nals if n]
        out: List[bytes] = []
        for k, nal in enumerate(nals):
            last_nal = k == len(nals) - 1
            if len(nal) <= MTU_PAYLOAD:
                out.append(self._hdr(last_nal, ts) + nal)
                continue
            # FU-A: indicator keeps F and NRI from the NAL header with type 28;
            # the FU header carries S/E bits and the original type.
            indicator = (nal[0] & 0xe0) | 28
            typ = nal[0] & 0x1f
            body = nal[1:]
            pos = 0
            first = True
            while pos < len(body):
                chunk = body[pos:pos + MTU_PAYLOAD - 2]
                pos += len(chunk)
                end = pos >= len(body)
                fu = (0x80 if first else 0) | (0x40 if end else 0) | typ
                out.append(self._hdr(last_nal and end, ts) + bytes([indicator, fu]) + chunk)
                first = False
        return out


class Relay:
    """
    Pictures in, RTP datagrams out, with the two rules from the module doc.
    `send(bytes)` is the socket; `now()` the monotonic clock the timestamps
    come from — arrival time, because the drone stamps nothing. Its jitter is
    the Wi-Fi's, bounded on the pilot by the profile's latency budget.
    """

    REREQUEST_S = 0.5

    def __init__(self, send: Callable[[bytes], None], now: Callable[[], float], on_need_keyframe: Callable[[], None],
                 resume_after_s: float = 1.5, after_loss: str = 'forward'):
        if after_loss not in ('forward', 'wait'):
            raise ValueError(f'after_loss must be forward or wait, not {after_loss!r}')
        self.after_loss = after_loss
        self.want_idr = False             # forward mode: a keyframe was asked for and has not arrived
        self.send = send
        self.now = now
        self.on_need_keyframe = on_need_keyframe
        self.resume_after_s = resume_after_s
        self.awaiting_since: Optional[float] = None
        self.kf_requested_at: Optional[float] = None
        self.pk = Packetizer()
        self.sps: Optional[bytes] = None
        self.pps: Optional[bytes] = None
        self.awaiting_idr = True          # nothing goes out before the first keyframe either
        self.t0: Optional[float] = None
        self.codec: Optional[str] = None
        self.stats = {'frames': 0, 'idr': 0, 'dropped_awaiting_idr': 0, 'param_only': 0, 'packets': 0, 'bytes': 0,
                      'losses': 0, 'resumed_without_idr': 0, 'idr_without_params': 0, 'forwarded_unrepaired': 0}
        self.last_idr_at: Optional[float] = None
        self.idr_intervals: List[float] = []

    def mark_loss(self, n: int = 1) -> None:
        self.stats['losses'] += n
        if self.awaiting_idr:
            return                        # already holding back and asking
        if self.after_loss == 'wait':
            self.awaiting_idr = True
            self.awaiting_since = self.now()
            self._request_keyframe()
        elif not self.want_idr:
            self.want_idr = True
            self._request_keyframe()

    def _request_keyframe(self) -> None:
        self.kf_requested_at = self.now()
        self.on_need_keyframe()

    def push_picture(self, data: bytes) -> Tuple[int, bool]:
        """Returns (packets sent, was_keyframe)."""
        nals = split_annexb(data)
        vcl: List[bytes] = []
        idr = False
        for nal in nals:
            t = nal[0] & 0x1f
            if t == NAL_SPS:
                if self.sps != nal:
                    self.sps = nal
                    codec = sps_codec_string(nal)
                    if codec != self.codec:
                        self.codec = codec
                        log.info('stream is %s (profile_idc=%d level_idc=%d) — the channel\'s codec string should say so',
                                 codec, nal[1], nal[3])
            elif t == NAL_PPS:
                self.pps = nal
            elif t in (NAL_NON_IDR, NAL_IDR):
                vcl.append(nal)
                idr = idr or t == NAL_IDR
            # SEI, AUD, filler: the browser needs none of them
        if not vcl:
            self.stats['param_only'] += 1
            return (0, False)
        if idr:
            now = self.now()
            if self.last_idr_at is not None:
                self.idr_intervals.append(now - self.last_idr_at)
                del self.idr_intervals[:-50]
            self.last_idr_at = now
            if not (self.sps and self.pps):
                # Seen on the real drone: the first keyframe can arrive before
                # its parameter sets. No decoder can start from it, so it is
                # not a keyframe worth forwarding; ask again, the sets will be
                # cached by the time the next one comes.
                self.stats['idr_without_params'] += 1
                self.awaiting_idr = True
                self.awaiting_since = None
                self._request_keyframe()
                return (0, False)
            self.stats['idr'] += 1
            self.awaiting_idr = False
            self.want_idr = False
            nals_out = [self.sps, self.pps] + vcl
        elif self.awaiting_idr:
            now = self.now()
            waited = now - self.awaiting_since if self.awaiting_since is not None else float('inf')
            if waited > self.resume_after_s and self.stats['idr'] > 0:
                # The drone did not answer. Forward deltas rather than stay
                # blind; the picture clears on the next keyframe, whenever it comes.
                log.warning('no keyframe %.1fs after the request — forwarding delta frames', waited)
                self.stats['resumed_without_idr'] += 1
                self.awaiting_idr = False
                nals_out = vcl
            else:
                if self.kf_requested_at is None or now - self.kf_requested_at >= self.REREQUEST_S:
                    self._request_keyframe()
                self.stats['dropped_awaiting_idr'] += 1
                return (0, False)
        else:
            if self.want_idr:
                # Forwarding past a loss: the decoder is concealing. Keep
                # asking until the keyframe that repairs it shows up.
                self.stats['forwarded_unrepaired'] += 1
                now = self.now()
                if self.kf_requested_at is None or now - self.kf_requested_at >= self.REREQUEST_S:
                    self._request_keyframe()
            nals_out = vcl
        now = self.now()
        if self.t0 is None:
            self.t0 = now
        ts = int((now - self.t0) * RTP_CLOCK_HZ) & 0xffffffff
        pkts = self.pk.packets(nals_out, ts)
        for p in pkts:
            self.send(p)
        self.stats['frames'] += 1
        self.stats['packets'] += len(pkts)
        self.stats['bytes'] += sum(len(p) for p in pkts)
        return (len(pkts), idr)
