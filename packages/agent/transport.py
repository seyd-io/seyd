"""
darc-agent WebTransport server (aioquic-based).

Accepts a single WebTransport session at a time (last-connect wins).
Video frames are sent as QUIC datagrams (unreliable, lowest latency).
Sensor data and command acknowledgements travel over a JSON line-delimited
bidirectional stream initiated by the pilot.
Commands from the pilot arrive on the same stream and are dispatched via
the on_message callback.

After the server starts, get_socket() returns the underlying UDP socket so
the caller can run STUN from it to discover the correct external address.
"""

import asyncio
import json
import logging
import socket
from typing import Callable, Optional

from aioquic.asyncio import serve
from aioquic.asyncio.protocol import QuicConnectionProtocol
from aioquic.h3.connection import H3Connection, H3_ALPN
from aioquic.h3.events import (
    DatagramReceived,
    HeadersReceived,
    WebTransportStreamDataReceived,
)
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import QuicEvent

log = logging.getLogger(__name__)

_PATH = b'/darc'


class _Session:
    """One active WebTransport connection from a pilot."""

    def __init__(self, session_id: int, http: H3Connection, protocol: 'DARCProtocol'):
        self._session_id = session_id
        self._http = http
        self._protocol = protocol
        self._stream_id: Optional[int] = None
        self._buf = b''
        self.on_message: Optional[Callable] = None

    def flush_datagrams(self):
        """Discard QUIC DATAGRAM frames still queued for transmission.

        Called before sending a new video frame so that stale chunks from the
        previous frame (which haven't left the host yet due to congestion or
        pacing) don't delay the new one.
        """
        try:
            self._protocol._quic._datagrams_pending.clear()
        except AttributeError:
            pass

    async def send_datagram(self, data: bytes):
        self._http.send_datagram(self._session_id, data)
        self._protocol.transmit()

    async def send_json(self, msg: dict):
        if self._stream_id is None:
            return
        payload = (json.dumps(msg) + '\n').encode()
        self._protocol._quic.send_stream_data(self._stream_id, payload, end_stream=False)
        self._protocol.transmit()

    def stream_data_received(self, stream_id: int, data: bytes):
        if self._stream_id is None:
            self._stream_id = stream_id
        if stream_id != self._stream_id:
            return
        self._buf += data
        while b'\n' in self._buf:
            line, self._buf = self._buf.split(b'\n', 1)
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if self.on_message:
                asyncio.ensure_future(self.on_message(msg))


class DARCProtocol(QuicConnectionProtocol):

    def __init__(self, *args, server: 'WebTransportServer', **kwargs):
        super().__init__(*args, **kwargs)
        self._server = server
        self._http: Optional[H3Connection] = None
        self._session: Optional[_Session] = None

    def quic_event_received(self, event: QuicEvent):
        if self._http is None:
            self._http = H3Connection(self._quic, enable_webtransport=True)

        for http_event in self._http.handle_event(event):
            if isinstance(http_event, HeadersReceived):
                headers = dict(http_event.headers)
                if (headers.get(b':method') == b'CONNECT'
                        and headers.get(b':protocol') == b'webtransport'
                        and headers.get(b':path', b'').rstrip(b'/') == _PATH):
                    self._accept(http_event.stream_id)

            elif isinstance(http_event, WebTransportStreamDataReceived):
                if self._session:
                    self._session.stream_data_received(
                        http_event.stream_id, http_event.data
                    )

            elif isinstance(http_event, DatagramReceived):
                pass

    def _accept(self, stream_id: int):
        log.info('WebTransport session accepted (stream %d)', stream_id)
        self._http.send_headers(
            stream_id=stream_id,
            headers=[
                (b':status', b'200'),
                (b'sec-webtransport-http3-draft', b'draft02'),
            ],
        )
        self.transmit()

        session = _Session(session_id=stream_id, http=self._http, protocol=self)
        session.on_message = self._server.on_message
        self._session = session
        self._server._set_session(session)

    def connection_lost(self, exc):
        super().connection_lost(exc)
        if self._session and self._server._current_session is self._session:
            log.info('WebTransport session closed')
            self._server._set_session(None)
            self._session = None


class WebTransportServer:
    """
    Manages an aioquic WebTransport server.

    Callbacks (all async):
      on_connected(session)  — a pilot opened a WebTransport session
      on_disconnected()      — that session closed
      on_message(msg)        — JSON command received from the pilot
    """

    def __init__(self):
        self._current_session: Optional[_Session] = None
        self._quic_server = None
        self.on_connected: Optional[Callable] = None
        self.on_disconnected: Optional[Callable] = None
        self.on_message: Optional[Callable] = None

    async def start(self, port: int, cert, key):
        config = QuicConfiguration(
            alpn_protocols=H3_ALPN,
            is_client=False,
            max_datagram_frame_size=65536,
        )
        config.certificate = cert
        config.private_key = key

        self._quic_server = await serve(
            host='0.0.0.0',
            port=port,
            configuration=config,
            create_protocol=lambda *a, **kw: DARCProtocol(*a, server=self, **kw),
        )
        log.info('WebTransport server listening on port %d', port)

    def get_socket(self) -> Optional[socket.socket]:
        """Return the raw UDP socket so STUN can be run from it."""
        try:
            return self._quic_server._transport.get_extra_info('socket')
        except Exception:
            return None

    def probe(self, ip: str, ports: tuple[int, ...] = (443, 4433, 8080)):
        """
        Send small UDP packets from the WebTransport socket to ip on several
        common ports. This creates a NAT mapping on our side so the pilot's
        inbound QUIC packets are allowed through (address-restricted NAT).
        We probe multiple ports because we don't know which source port the
        pilot's browser will use for WebTransport.
        """
        sock = self.get_socket()
        if not sock:
            log.warning('probe: no socket available')
            return
        for port in ports:
            try:
                sock.sendto(b'\x00', (ip, port))
            except Exception as e:
                log.debug('probe to %s:%d failed: %s', ip, port, e)
        log.info('probed %s on ports %s to open NAT hole', ip, ports)

    def stop(self):
        if self._quic_server:
            self._quic_server.close()

    def _set_session(self, session: Optional[_Session]):
        self._current_session = session
        if session is not None:
            if self.on_connected:
                asyncio.ensure_future(self.on_connected(session))
        else:
            if self.on_disconnected:
                asyncio.ensure_future(self.on_disconnected())
