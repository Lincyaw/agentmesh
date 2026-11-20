import json
import base64
import hashlib
import os
from pathlib import Path
import select
import shlex
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
import uuid


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('AGENTMESH_BINARY', ROOT / 'target/debug/agentmesh'))


def wait_for(predicate, timeout=20):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            result = predicate()
        except (OSError, RuntimeError) as error:
            last_error = error
            result = None
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f'condition did not become true before timeout: {last_error}')


def rpc(directory, **request):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(25)
        client.connect(str(directory / 'daemon.sock'))
        client.sendall(json.dumps(request).encode() + b'\n')
        response = json.loads(client.makefile('rb').readline())
    if not response['ok']:
        raise RuntimeError(response['error'])
    return response['result']


class McpClient:
    def __init__(self, directory, name, harness='generic', session=None, environment=None):
        args = [str(BINARY), '--data-dir', str(directory), 'mcp', 'serve', '--name', name, '--harness', harness]
        if session:
            args += ['--session', session]
        self.log = open(directory / f'{name}.log', 'a')
        self.process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, env=environment)
        self.sequence = 0
        self.call('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {}, 'clientInfo': {'name': 'interop-test', 'version': '1.0'}})
        self.process.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.process.stdin.flush()
        self.session = self.tool('whoami')['session']

    def call(self, method, params):
        self.sequence += 1
        identifier = self.sequence
        self.process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': identifier, 'method': method, 'params': params}).encode() + b'\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.process.stdout], [], [], 1)
            if ready:
                line = self.process.stdout.readline()
                if not line:
                    raise AssertionError('MCP subprocess exited; inspect its log')
                response = json.loads(line)
                if response.get('id') == identifier:
                    if 'error' in response:
                        raise RuntimeError(response['error'])
                    return response['result']
        raise AssertionError('MCP request timed out')

    def tool(self, tool_name, **arguments):
        result = self.call('tools/call', {'name': tool_name, 'arguments': arguments})
        text = '\n'.join(item.get('text', '') for item in result.get('content', []))
        if result.get('isError'):
            raise RuntimeError(text)
        return json.loads(text)

    def inbox(self):
        return self.tool('check_inbox')

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process.stdout.close()
        self.log.close()


class Interoperability(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='am-', dir='/tmp')
        self.root = Path(self.temp.name)
        self.nodes = {}
        self.clients = []

    def tearDown(self):
        for client in self.clients:
            client.close()
        for node in self.nodes.values():
            self.stop(node)
            node['log'].close()
        self.temp.cleanup()

    def start(self, name):
        if name in self.nodes:
            node = self.nodes[name]
        else:
            directory = self.root / name
            directory.mkdir(mode=0o700)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as port:
                port.bind(('127.0.0.1', 0))
                number = port.getsockname()[1]
            node = {'dir': directory, 'port': number, 'log': open(directory / 'daemon.log', 'a')}
            self.nodes[name] = node
        args = [str(BINARY), '--data-dir', str(node['dir']), 'daemon', 'run', '--name', name, '--bind-addr', f'127.0.0.1:{node["port"]}']
        relay = os.environ.get('AGENTMESH_RELAY_URL')
        args += ['--relay-url', relay] if relay else ['--no-relay']
        if os.environ.get('AGENTMESH_RELAY_TOKEN_FILE'):
            args += ['--relay-token-file', os.environ['AGENTMESH_RELAY_TOKEN_FILE']]
        if os.environ.get('AGENTMESH_RELAY_ONLY'):
            args += ['--relay-only']
        node['process'] = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=node['log'])
        node['identity'] = wait_for(lambda: rpc(node['dir'], op='status')['node'] if (node['dir'] / 'daemon.sock').exists() else None)
        if os.environ.get('AGENTMESH_RELAY_ONLY'):
            wait_for(lambda: rpc(node['dir'], op='status')['address']['addrs'], timeout=30)
        return node

    def stop(self, node):
        process = node.get('process')
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)

    def client(self, node, name, harness='generic', session=None):
        client = McpClient(node['dir'], name, harness, session)
        self.clients.append(client)
        return client

    def pair(self, a, b):
        for receiver, sender in [(a, b), (b, a)]:
            ticket = rpc(sender['dir'], op='ticket')['ticket']
            rpc(receiver['dir'], op='peer_add', ticket=ticket)

    def test_iroh_mcp_direct_reply_groups_and_restart(self):
        # REQ-001 through REQ-005: real independent daemons and MCP subprocesses.
        a, b = self.start('A'), self.start('B')
        alice = self.client(a, 'alice', 'claude')
        bob = self.client(b, 'bob', 'codex')
        carol = self.client(a, 'carol')
        tools = alice.call('tools/list', {})['tools']
        self.assertIn('send_message', {tool['name'] for tool in tools})
        rpc(a['dir'], op='peer_add', ticket=rpc(b['dir'], op='ticket')['ticket'])
        self.assertEqual(len(alice.tool('list_sessions')['unreachable']), 1)
        self.pair(a, b)
        directory = alice.tool('list_sessions')
        self.assertEqual(len(directory['sessions']), 3)
        self.assertFalse(directory['unreachable'])

        direct = alice.tool('send_message', to=[bob.session['address']], body='请运行测试 🚀')
        received = wait_for(bob.inbox)
        self.assertEqual(received[0]['message']['body'], '请运行测试 🚀')
        self.assertEqual(received[0]['message']['from'], alice.session['address'])
        original = received[0]['message']
        response = bob.tool('send_message', to=[original['from']], body='测试通过', conversation=original['conversation'], reply_to=original['id'])
        reply = wait_for(alice.inbox)[0]['message']
        self.assertEqual(reply['reply_to'], direct['id'])
        self.assertEqual(reply['conversation'], direct['conversation'])
        self.assertEqual(bob.tool('acknowledge_messages', messages=[direct['id']])['acknowledged'], 1)
        self.assertEqual(bob.inbox(), [])
        alice.tool('acknowledge_messages', messages=[response['id']])

        alice.tool('set_group', name='team', members=[bob.session['address'], carol.session['address']])
        group = alice.tool('send_message', group='team', body='group announcement')
        self.assertEqual(wait_for(bob.inbox)[0]['message']['id'], group['id'])
        self.assertEqual(wait_for(carol.inbox)[0]['message']['id'], group['id'])
        bob.tool('acknowledge_messages', messages=[group['id']])
        carol.tool('acknowledge_messages', messages=[group['id']])
        wait_for(lambda: all(d['status'] == 'delivered' for d in rpc(a['dir'], op='deliveries')))
        # Simulate a sender crash after the receiver stored a message but before receipt persistence.
        with sqlite3.connect(a['dir'] / 'mailbox.sqlite') as db:
            db.execute("UPDATE outbox SET status='pending',next_attempt=0 WHERE id=?", (group['id'],))
        wait_for(lambda: all(d['status'] == 'delivered' for d in rpc(a['dir'], op='deliveries')))
        self.assertEqual(bob.inbox(), [])
        self.assertEqual(carol.inbox(), [])

        identity = b['identity']
        self.stop(b)
        queued = alice.tool('send_message', to=[bob.session['address']], body='while offline')
        self.assertEqual(queued['status'], 'queued')
        self.stop(a)
        self.start('A')
        self.start('B')
        self.assertEqual(b['identity'], identity)
        offline = wait_for(bob.inbox, timeout=35)
        self.assertEqual(len(offline), 1)
        self.assertEqual(offline[0]['message']['id'], queued['id'])
        self.assertEqual(alice.tool('list_groups')['groups'][0]['name'], 'team')
        with self.assertRaises(RuntimeError):
            alice.tool('send_message', to=[bob.session['address']], body='')
        with self.assertRaises(RuntimeError):
            alice.tool('check_inbox', limit=0)

    def test_four_harness_families_exchange_messages(self):
        a,b=self.start('A'),self.start('B')
        self.pair(a,b)
        clients=[self.client(a,'claude','claude'),self.client(b,'tcodex','tcodex'),self.client(a,'tclaude','tclaude'),self.client(b,'codex','codex')]
        for i,sender in enumerate(clients):
            target=clients[(i+1)%len(clients)]
            sent=sender.tool('send_message',to=[target.session['address']],body=sender.session['harness'])
            received=wait_for(target.inbox)[0]['message']
            self.assertEqual(received['id'],sent['id'])
            self.assertEqual(received['body'],sender.session['harness'])
            target.tool('acknowledge_messages',messages=[sent['id']])

    def test_session_recovers_after_daemon_outage_exceeding_lease(self):
        node = self.start('A')
        client = self.client(node, 'persistent')
        self.stop(node)
        time.sleep(46)
        self.start('A')
        def usable():
            client.tool('check_inbox')
            return True
        self.assertTrue(wait_for(usable, timeout=15))

    def test_stable_session_successor_keeps_credential_file(self):
        node = self.start('A')
        identifier = str(uuid.uuid4())
        old = self.client(node, 'old', session=identifier)
        with sqlite3.connect(node['dir'] / 'mailbox.sqlite') as db:
            db.execute('UPDATE sessions SET seen=? WHERE id=?', (int(time.time() * 1000) - 60_000, identifier))
        successor = self.client(node, 'successor', session=identifier)
        files = list(node['dir'].glob(f'session-{identifier}-*.json'))
        self.assertEqual(len(files), 2)
        old.close()
        self.assertEqual(len(list(node['dir'].glob(f'session-{identifier}-*.json'))), 1)
        self.assertEqual(successor.inbox(), [])

    def test_large_inbox_reads_stay_within_frame_budget(self):
        node = self.start('A')
        sender, recipient = self.client(node, 'sender'), self.client(node, 'recipient')
        ids = set()
        for _ in range(10):
            ids.add(sender.tool('send_message', to=[recipient.session['address']], body='\x01' * 64000)['id'])
        wait_for(lambda: all(d['status'] == 'delivered' for d in rpc(node['dir'], op='deliveries')))
        seen = set()
        while seen != ids:
            page = recipient.tool('check_inbox', limit=100)
            self.assertTrue(page)
            self.assertLess(len(json.dumps(page).encode()), 2 * 1024 * 1024)
            page_ids = [item['message']['id'] for item in page]
            self.assertFalse(seen.intersection(page_ids))
            recipient.tool('acknowledge_messages', messages=page_ids)
            seen.update(page_ids)

    def test_directory_returns_partial_results_with_many_offline_peers(self):
        node = self.start('A')
        client = self.client(node, 'local')
        template = json.loads(base64.urlsafe_b64decode(rpc(node['dir'], op='ticket')['ticket'] + '==='))
        accepted = 0
        attempt = 0
        while accepted < 24:
            attempt += 1
            template['address']['id'] = hashlib.sha256(f'offline-{attempt}'.encode()).hexdigest()
            template['address']['addrs'] = [{'Ip': '127.0.0.1:9'}]
            ticket = base64.urlsafe_b64encode(json.dumps(template).encode()).decode().rstrip('=')
            try:
                rpc(node['dir'], op='peer_add', ticket=ticket)
            except RuntimeError:
                if attempt > 200:
                    raise
                continue
            accepted += 1
        started = time.monotonic()
        directory = client.tool('list_sessions')
        self.assertLess(time.monotonic() - started, 18)
        self.assertEqual(len(directory['unreachable']), 24)
        self.assertEqual(directory['sessions'][0]['id'], client.session['id'])

    def test_native_wake_adapters_contract(self):
        # REQ-006: local protocol fixtures verify adapter payloads, not model execution.
        node = self.start('A')
        sender = self.client(node, 'sender')
        claude = self.client(node, 'claude', 'claude')
        codex = self.client(node, 'codex', 'codex')
        address = self.root / 'inbox.sock'
        captured = []
        ready = threading.Event()
        def receive():
            with socket.socket(socket.AF_UNIX) as server:
                server.bind(str(address))
                os.chmod(address, 0o600)
                server.listen(1)
                server.settimeout(20)
                ready.set()
                client, _ = server.accept()
                with client:
                    captured.extend(json.loads(line) for line in client.makefile('rb'))
        listener = threading.Thread(target=receive, daemon=True)
        listener.start()
        self.assertTrue(ready.wait(5))
        rpc(node['dir'], op='attach', id=claude.session['id'], target={'kind': 'claude', 'socket': str(address), 'token': 'local-fixture-token'})
        message = sender.tool('send_message', to=[claude.session['address']], body='body stays in inbox')
        wait_for(lambda: len(captured) == 2)
        self.assertEqual(captured[0], {'type': 'auth', 'token': 'local-fixture-token'})
        reminder = captured[1]['message']['content']
        self.assertIn(message['id'], reminder)
        self.assertNotIn('body stays in inbox', reminder)
        self.assertIn('not user approval', reminder)
        listener.join(timeout=5)

        executable = self.root / 'codex-protocol-fixture'
        output = self.root / 'codex-args'
        executable.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > ' + shlex.quote(str(output)) + '\n')
        executable.chmod(0o700)
        thread = str(uuid.uuid4())
        rpc(node['dir'], op='attach', id=codex.session['id'], target={'kind': 'codex', 'binary': str(executable), 'thread': thread})
        queued = sender.tool('send_message', to=[codex.session['address']], body='codex payload')
        args = wait_for(lambda: output.read_text().splitlines() if output.exists() else None)
        self.assertEqual(args[:4], ['queue', '--thread', thread, '--message'])
        self.assertIn(queued['id'], args[4])
        self.assertNotIn('codex payload', args[4])


if __name__ == '__main__':
    unittest.main(verbosity=2)
