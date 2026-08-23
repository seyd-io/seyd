"""
TLS certificate generation for the darc-agent WebTransport server.

Generates a self-signed ECDSA P-256 cert on startup. Requirements for Chrome's
serverCertificateHashes API (which lets the pilot trust the cert by fingerprint
without a CA chain):

  • ECDSA P-256 key
  • Validity ≤ 14 days (we use 13 to avoid precision boundary issues)
  • Must include a SubjectAlternativeName extension
  • SHA-256 fingerprint (of DER-encoded cert) must match what the pilot supplies

The cert is regenerated on every agent startup. The fingerprint is registered
with the signal server and forwarded to the pilot via the ready message.
"""

import datetime
import hashlib
import ipaddress

from cryptography import x509
from cryptography.x509.oid import NameOID
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec


def generate_cert(ip_addresses: list[str] | None = None):
    """
    Returns (cert, private_key, fingerprint_hex).

    ip_addresses: list of IPv4 strings to include in SubjectAlternativeName.
    fingerprint_hex: lowercase hex SHA-256 of the DER cert — matches what
                     Chrome computes for serverCertificateHashes.
    """
    key  = ec.generate_private_key(ec.SECP256R1())
    now  = datetime.datetime.now(datetime.timezone.utc)

    # Build SubjectAlternativeName. Chrome requires SAN to be present for the
    # fingerprint verifier to accept the cert. Include every IP the agent may
    # be reachable on so the cert is nominally "correct" for the URLs tried.
    san_ips: list[x509.GeneralName] = []
    for ip in (ip_addresses or []):
        try:
            san_ips.append(x509.IPAddress(ipaddress.IPv4Address(ip)))
        except ValueError:
            pass
    if not san_ips:
        san_ips.append(x509.IPAddress(ipaddress.IPv4Address('127.0.0.1')))

    cert = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'darc-agent')]))
        .issuer_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'darc-agent')]))
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now.replace(tzinfo=None))
        .not_valid_after((now + datetime.timedelta(days=13)).replace(tzinfo=None))
        .add_extension(x509.SubjectAlternativeName(san_ips), critical=False)
        .sign(key, hashes.SHA256())
    )

    # Compute fingerprint from the exact DER bytes that aioquic will send in
    # the TLS Certificate message — guaranteeing it matches Chrome's computation.
    cert_der     = cert.public_bytes(serialization.Encoding.DER)
    fingerprint  = hashlib.sha256(cert_der).hexdigest()

    return cert, key, fingerprint
