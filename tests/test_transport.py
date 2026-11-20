"""Observe DNS requests from real endpoints using an isolated local DNS server."""
import base64
import json
import os
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

from test_interop import BINARY, rpc, wait_for


class TransportPrivacy(unittest.TestCase):
    def test_no_public_discovery_and_only_explicit_relay_hostname_is_resolved(self):
        queries = []
        stop = threading.Event()
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as dns:
            dns.bind(('127.0.0.1', 0))
            dns.settimeout(0.1)
            def respond():
                while not stop.is_set():
                    try:
                        packet, address = dns.recvfrom(4096)
                    except socket.timeout:
                        continue
                    labels, offset = [], 12
                    while packet[offset]:
                        size = packet[offset]
                        labels.append(packet[offset + 1:offset + 1 + size].decode('ascii'))
                        offset += size + 1
                    offset += 1
                    qtype = struct.unpack('!H', packet[offset:offset + 2])[0]
                    queries.append('.'.join(labels).lower())
                    question = packet[12:offset + 4]
                    answer = b''
                    if qtype == 1:
                        answer = b'\xc0\x0c' + struct.pack('!HHIH', 1, 1, 0, 4) + socket.inet_aton('127.0.0.1')
                    header = packet[:2] + struct.pack('!HHHHH', 0x8180, 1, bool(answer), 0, 0)
                    dns.sendto(header + question + answer, address)
            thread = threading.Thread(target=respond, daemon=True)
            thread.start()
            try:
                with tempfile.TemporaryDirectory(prefix='am-dns-', dir='/tmp') as temp:
                    root = Path(temp)
                    def launch(name, relay=False, peer=None):
                        directory = root / name
                        log = open(root / f'{name}.log', 'w')
                        args = [str(BINARY), '--data-dir', str(directory), 'daemon', 'run', '--name', name,
                                '--bind-addr', '127.0.0.1:0', '--dns-server', f'127.0.0.1:{dns.getsockname()[1]}']
                        if relay:
                            args += ['--relay-url', 'https://private-relay.test:9', '--relay-only']
                        process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=log)
                        try:
                            wait_for(lambda: rpc(directory, op='status') if (directory / 'daemon.sock').exists() else None)
                            if relay:
                                wait_for(lambda: queries)
                                template = rpc(directory, op='ticket')['ticket']
                                ticket = json.loads(base64.urlsafe_b64decode(template + '==='))
                                ticket['address']['id'] = peer['node']
                                ticket['name'] = 'unapproved-relay'
                                with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as forbidden:
                                    forbidden.bind(('127.0.0.1', 0))
                                    forbidden.listen(4)
                                    forbidden.settimeout(0.2)
                                    ticket['address']['addrs'] = [
                                        {'Relay':'https://unexpected-peer-relay.test:9/'},
                                        {'Relay':f'http://127.0.0.1:{forbidden.getsockname()[1]}/'},
                                    ]
                                    encoded = base64.urlsafe_b64encode(json.dumps(ticket).encode()).decode().rstrip('=')
                                    rpc(directory, op='peer_add', ticket=encoded)
                                    result = rpc(directory, op='workspace_list', node=peer['node'])
                                    self.assertIn('transport policy', result['unreachable'][0]['error'])
                                    invitation = {'version':1,'inviter':ticket,'secret':base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip('='),'expires_at':int(time.time()*1000)+60000}
                                    code = 'am1_' + base64.urlsafe_b64encode(json.dumps(invitation).encode()).decode().rstrip('=')
                                    with self.assertRaisesRegex(RuntimeError, 'transport policy'):
                                        rpc(directory, op='join', code=code)
                                    with self.assertRaises(socket.timeout):
                                        forbidden.accept()
                            else:
                                time.sleep(1.5)
                            return rpc(directory, op='status')
                        finally:
                            process.terminate()
                            process.wait(timeout=10)
                            log.close()
                    # No --no-relay flag: the actual default must make no discovery query.
                    peer = launch('default')
                    self.assertEqual(queries, [])
                    launch('custom', relay=True, peer=peer)
                    self.assertTrue(queries)
                    self.assertEqual(set(queries), {'private-relay.test'})
            finally:
                stop.set()
                thread.join(timeout=2)


if __name__ == '__main__':
    unittest.main(verbosity=2)
