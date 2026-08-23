"""
Simulates a robot sensor publishing a counter over UDP.
The DARC Agent subscribes to this port and forwards each packet to the pilot.

This is prototype scaffolding — not part of DARC.
"""

import asyncio
import socket
import argparse


def parse_args():
    p = argparse.ArgumentParser(description='Simulated sensor source')
    p.add_argument('--port', type=int, default=5002)
    p.add_argument('--rate', type=float, default=10.0, help='Hz')
    return p.parse_args()


async def main():
    args = parse_args()
    interval = 1.0 / args.rate
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    counter = 0

    print(f'Sending counter → UDP 127.0.0.1:{args.port} at {args.rate} Hz')
    print('Press Ctrl+C to stop.')

    try:
        while True:
            sock.sendto(str(counter).encode(), ('127.0.0.1', args.port))
            counter += 1
            await asyncio.sleep(interval)
    except KeyboardInterrupt:
        pass
    finally:
        sock.close()


if __name__ == '__main__':
    asyncio.run(main())
