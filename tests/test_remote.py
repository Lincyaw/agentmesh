"""Real iroh nodes + MCP processes: remote workspaces, durable jobs, and access boundaries."""
import json
import os
import socket
import subprocess
import time
import unittest
import uuid

import test_interop as fixtures
from test_interop import BINARY, rpc, wait_for


TERMINAL = {'succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted'}


class RemoteWorkspaces(unittest.TestCase):
    setUp = fixtures.Interoperability.setUp
    tearDown = fixtures.Interoperability.tearDown
    start = fixtures.Interoperability.start
    stop = fixtures.Interoperability.stop
    pair = fixtures.Interoperability.pair
    client = fixtures.Interoperability.client

    def prepare(self, write=False, execute=False, grant=True):
        a, b = self.start('laptop'), self.start('server')
        self.pair(a, b)
        root = self.root / 'project'
        root.mkdir()
        root = root.resolve()
        (root / 'hello.txt').write_text('hello remote world\n', encoding='utf-8')
        self.admin(b, 'add', name='project', path=str(root))
        if grant:
            self.grant(b, a, write=write, execute=execute)
        return a, b, root, self.client(a, 'local-agent')

    def admin(self, node, action, **args):
        return rpc(node['dir'], op='workspace_admin', command={'action': action, **args})

    def grant(self, host, peer, write=False, execute=False):
        return self.admin(host, 'grant', name='project', peer=peer['identity'], write=write, execute=execute)

    def run_job(self, agent, command, **args):
        return agent.tool('remote_execute', node='server', workspace='project', command=command,
                          request_id=args.pop('request_id', str(uuid.uuid4())), **args)

    def result(self, agent, job):
        def complete():
            result = agent.tool('remote_job', node='server', job=job['id'])
            return result if result['status'] in TERMINAL else None
        return wait_for(complete)

    def test_explicit_grants_and_machine_job_ownership(self):
        a, b, root, agent = self.prepare(grant=False)
        machines = agent.tool('remote_workspaces', node='server')['machines']
        self.assertEqual(machines[0]['workspaces'], [])
        with self.assertRaisesRegex(RuntimeError, 'not authorized'):
            agent.tool('remote_read_file', node='server', workspace='project', path='hello.txt')
        with self.assertRaises(RuntimeError):
            self.run_job(agent, 'touch denied')
        self.assertFalse((root / 'denied').exists())
        self.grant(b, a)
        self.assertIn('hello remote', agent.tool('remote_read_file', node='server', workspace='project', path='hello.txt')['content'])
        with self.assertRaisesRegex(RuntimeError, 'write permission'):
            agent.tool('remote_write_file', node='server', workspace='project', path='new.txt', content='x')
        with self.assertRaisesRegex(RuntimeError, 'execute permission'):
            self.run_job(agent, 'true')
        self.grant(b, a, execute=True)
        job = self.run_job(agent, 'printf private')
        self.assertEqual(self.result(agent, job)['stdout']['text'], 'private')
        c = self.start('other')
        self.pair(c, b)
        self.grant(b, c, execute=True)
        other = self.client(c, 'other-agent')
        with self.assertRaisesRegex(RuntimeError, 'not authorized'):
            other.tool('remote_job', node='server', job=job['id'])
        with self.assertRaises(RuntimeError):
            other.tool('remote_cancel_job', node='server', job=job['id'])
        self.assertEqual(other.tool('remote_jobs', node='server', workspace='project')['jobs'], [])

    def test_four_harnesses_files_search_and_compare_before_replace(self):
        a, b, root, agent = self.prepare(write=True)
        for harness in ['claude', 'tclaude', 'codex', 'tcodex']:
            client = self.client(a, harness, harness)
            tools = {t['name'] for t in client.call('tools/list', {})['tools']}
            self.assertIn('remote_execute', tools)
            data = client.tool('remote_read_file', node='server', workspace='project', path='hello.txt')
            self.assertEqual(data['content'], 'hello remote world\n')
        listing = agent.tool('remote_list_files', node='server', workspace='project')
        self.assertEqual(listing['entries'][0]['name'], 'hello.txt')
        args = dict(node='server', workspace='project', path='new.txt', content='你好 🚀\n')
        self.assertTrue(agent.tool('remote_write_file', **args)['changed'])
        self.assertFalse(agent.tool('remote_write_file', **args)['changed'])
        old = agent.tool('remote_read_file', node='server', workspace='project', path='new.txt')
        args['content'] = 'updated\n'
        with self.assertRaises(RuntimeError):
            agent.tool('remote_write_file', **args)
        agent.tool('remote_write_file', expected_sha256=old['sha256'], **args)
        with self.assertRaises(RuntimeError):
            agent.tool('remote_write_file', expected_sha256=old['sha256'], **{**args, 'content': 'stale'})
        (root / 'nested').mkdir()
        (root / 'nested/file.txt').write_text('needle here\n')
        results = agent.tool('remote_search', node='server', workspace='project', query='needle')
        self.assertEqual(results['matches'][0]['path'], 'nested/file.txt')
        self.assertEqual(results['matches'][0]['line'], 1)

    def test_path_symlink_special_file_and_size_boundaries(self):
        a, b, root, agent = self.prepare(write=True)
        secret = self.root / 'secret.txt'
        secret.write_text('secret')
        (root / 'link').symlink_to(secret)
        (root / 'dirlink').symlink_to(self.root, target_is_directory=True)
        os.mkfifo(root / 'pipe')
        for path in ['../secret.txt', str(secret), 'link', 'dirlink/secret.txt', 'pipe']:
            with self.assertRaises(RuntimeError):
                agent.tool('remote_read_file', node='server', workspace='project', path=path)
        for path in ['../new.txt', 'link', 'dirlink/new.txt', 'pipe']:
            with self.assertRaises(RuntimeError):
                agent.tool('remote_write_file', node='server', workspace='project', path=path, content='bad')
        self.assertEqual(secret.read_text(), 'secret')
        (root / 'big').write_bytes(b'x' * 65537)
        (root / 'binary').write_bytes(b'\xff\xfe')
        for path in ['big', 'binary']:
            with self.assertRaises(RuntimeError):
                agent.tool('remote_read_file', node='server', workspace='project', path=path)
        with self.assertRaises(RuntimeError):
            agent.tool('remote_write_file', node='server', workspace='project', path='too-big', content='x' * 65537)
        # Search must skip the FIFO, not wait for a writer.
        agent.tool('remote_search', node='server', workspace='project', query='hello')

    def test_async_jobs_retry_conflict_output_and_reconnect(self):
        a, b, root, agent = self.prepare(execute=True)
        request = dict(workspace='project', path='.', command='printf x >> count; printf first; printf error >&2; sleep 1; printf second; exit 7', request_id='stable-request', timeout_seconds=10)
        # Submit and deliberately lose the response; retry must find the same durable job.
        with socket.socket(socket.AF_UNIX) as client:
            client.connect(str(a['dir'] / 'daemon.sock'))
            client.sendall(json.dumps({'op': 'workspace_call', 'node': 'server', 'request': {'action': 'execute', **request}}).encode() + b'\n')
        wait_for(lambda: (root / 'count').exists())
        job = agent.tool('remote_execute', node='server', **request)
        again = agent.tool('remote_execute', node='server', **request)
        self.assertEqual(job['id'], again['id'])
        with self.assertRaisesRegex(RuntimeError, 'different inputs'):
            agent.tool('remote_execute', node='server', **{**request, 'command': 'true'})
        agent.close()
        replacement = self.client(a, 'replacement')
        result = self.result(replacement, job)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['exit_code'], 7)
        self.assertEqual(result['stdout']['text'], 'firstsecond')
        self.assertEqual(result['stderr']['text'], 'error')
        self.assertEqual((root / 'count').read_text(), 'x')
        tail = replacement.tool('remote_job', node='server', job=job['id'], stdout_cursor=5, stderr_cursor=5)
        self.assertEqual(tail['stdout']['text'], 'second')
        self.assertEqual(tail['stderr']['text'], '')
        self.assertEqual(replacement.tool('remote_jobs', node='server', workspace='project')['jobs'][0]['id'], job['id'])

    def test_timeout_cancel_and_revocation_kill_process_groups(self):
        a, b, root, agent = self.prepare(execute=True)
        timed = self.run_job(agent, 'sleep 30', timeout_seconds=1)
        self.assertEqual(self.result(agent, timed)['status'], 'timed_out')
        job = self.run_job(agent, '(sleep 2; touch escaped) & echo ready; wait')
        wait_for(lambda: agent.tool('remote_job', node='server', job=job['id'])['stdout']['text'])
        agent.tool('remote_cancel_job', node='server', job=job['id'])
        self.assertEqual(self.result(agent, job)['status'], 'cancelled')
        time.sleep(2.1)
        self.assertFalse((root / 'escaped').exists())
        job = self.run_job(agent, 'sleep 30')
        self.admin(b, 'revoke', name='project', peer=a['identity'])
        with self.assertRaises(RuntimeError):
            agent.tool('remote_job', node='server', job=job['id'])
        local = lambda: rpc(b['dir'], op='workspace_call', node='local', request={'action': 'job', 'job': job['id'], 'stdout_cursor': 0, 'stderr_cursor': 0})
        self.assertEqual(wait_for(lambda: local() if local()['status'] in TERMINAL else None)['status'], 'cancelled')

    def test_output_caps_and_raw_byte_cursors(self):
        a, b, root, agent = self.prepare(execute=True)
        job = self.run_job(agent, 'head -c 1100000 /dev/zero; head -c 1100000 /dev/zero >&2')
        result = self.result(agent, job)
        self.assertTrue(result['stdout_truncated'])
        self.assertTrue(result['stderr_truncated'])
        self.assertEqual(result['stdout']['bytes'], 1048576)
        self.assertEqual(result['stderr']['bytes'], 1048576)
        self.assertEqual(result['stdout']['next_cursor'], 65536)
        self.assertEqual(len(result['stdout']['text']), 65536)
        with self.assertRaises(RuntimeError):
            agent.tool('remote_job', node='server', job=job['id'], stdout_cursor=1048577)

    def test_detached_descendant_cannot_hold_job_open_past_timeout(self):
        a, b, root, agent = self.prepare(execute=True)
        script = 'import os,time; os.setsid(); print("detached",flush=True); time.sleep(4)'
        import shlex
        started = time.monotonic()
        job = self.run_job(agent, 'python3 -c ' + shlex.quote(script) + ' & sleep 30', timeout_seconds=1)
        result = self.result(agent, job)
        self.assertEqual(result['status'], 'timed_out')
        self.assertLess(time.monotonic() - started, 3.5)
        self.assertTrue(result['stdout_truncated'])

        self.assertIn('incomplete', result['error'])
        # A finite descendant may escape its process group, but cannot pin capture.
        time.sleep(max(0, 4.2 - (time.monotonic() - started)))

    def test_unicode_output_preserves_codepoints_across_pages_and_polls(self):
        a, b, root, agent = self.prepare(execute=True)
        job = self.run_job(agent, "python3 -c 'print(\"a\"*65535+\"你好\",end=\"\")'")
        first = self.result(agent, job)
        second = agent.tool('remote_job', node='server', job=job['id'], stdout_cursor=first['stdout']['next_cursor'])
        self.assertEqual(first['stdout']['text'] + second['stdout']['text'], 'a' * 65535 + '你好')
        self.assertEqual(first['stdout']['next_cursor'], 65535)
        command = "python3 -c 'import os,time; os.write(1,bytes([228])); time.sleep(0.6); os.write(1,bytes([189,160]))'"
        streaming = self.run_job(agent, command)
        seen = wait_for(lambda: agent.tool('remote_job', node='server', job=streaming['id']) if agent.tool('remote_job', node='server', job=streaming['id'])['stdout']['bytes'] else None)
        self.assertNotIn('�', seen['stdout']['text'])
        self.assertEqual(self.result(agent, streaming)['stdout']['text'], '你')

    def test_restart_persistence_and_interrupted_jobs_are_not_replayed(self):
        a, b, root, agent = self.prepare(execute=True)
        job = self.run_job(agent, 'printf persistent')
        self.result(agent, job)
        pending = self.run_job(agent, 'printf x >> once; sleep 1', request_id='do-not-replay')
        wait_for(lambda: (root / 'once').exists())
        b['process'].kill()
        b['process'].wait(timeout=10)
        self.start('server')
        self.assertEqual(self.result(agent, job)['stdout']['text'], 'persistent')
        interrupted = self.result(agent, pending)
        self.assertEqual(interrupted['status'], 'interrupted')
        again = self.run_job(agent, 'printf x >> once; sleep 1', request_id='do-not-replay')
        self.assertEqual(again['id'], pending['id'])
        self.assertEqual((root / 'once').read_text(), 'x')
        # The finite subprocess from the forced crash exits independently.
        time.sleep(1.1)

    def test_cli_contract_and_retired_session_fencing(self):
        a, b, root, agent = self.prepare(write=True, execute=True)
        cli = lambda *args: json.loads(subprocess.check_output([str(BINARY), '--data-dir', str(a['dir']), *args]))
        self.assertEqual(cli('remote', 'read', '--node', 'server', '--workspace', 'project', '--path', 'hello.txt')['content'], 'hello remote world\n')
        listed = cli('remote', 'list')['machines']
        self.assertTrue(any(m['name'] == 'server' for m in listed))
        job = cli('remote', 'execute', '--node', 'server', '--workspace', 'project', '--command', 'pwd', '--request-id', 'cli-command')
        self.assertEqual(self.result(agent, job)['stdout']['text'].strip(), str(root))
        # Every remote MCP request includes the execution lease; stale bridges fail closed.
        files = list(a['dir'].glob(f'session-{agent.session["id"]}-*.json'))
        registration = json.loads(files[0].read_text())
        rpc(a['dir'], op='retire', id=agent.session['id'], execution=registration['execution'])
        with self.assertRaisesRegex(RuntimeError, 'execution'):
            agent.tool('remote_read_file', node='server', workspace='project', path='hello.txt')


if __name__ == '__main__':
    unittest.main(verbosity=2)
