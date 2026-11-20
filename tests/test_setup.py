import concurrent.futures
import json
from pathlib import Path
import subprocess
import socket
import threading
import tempfile
import unittest

from test_interop import BINARY, McpClient, rpc, wait_for


class SharedSetup(unittest.TestCase):
    def test_invalid_private_transport_profile_preserves_old_daemon(self):
        with tempfile.TemporaryDirectory(prefix='am-upgrade-', dir='/tmp') as root:
            root = Path(root)
            directory, config = root / 'node', root / 'home'
            prepared = self.invoke(directory, config, '--config-only')
            self.assertEqual(prepared.returncode, 0, prepared.stderr)
            requests = []
            stopping = threading.Event()
            with socket.socket(socket.AF_UNIX) as server:
                server.bind(str(directory / 'daemon.sock'))
                server.listen(8)
                server.settimeout(0.1)
                def serve_old_version():
                    while not stopping.is_set():
                        try:
                            client, _ = server.accept()
                        except socket.timeout:
                            continue
                        with client:
                            request = json.loads(client.makefile('rb').readline())
                            requests.append(request['op'])
                            value = {'ok': True, 'result': {'version': '0.0.0'}}
                            client.sendall(json.dumps(value).encode() + b'\n')
                thread = threading.Thread(target=serve_old_version, daemon=True)
                thread.start()
                try:
                    for invalid, message in [({'relay_only': True}, 'relay_url'), ({'dns_server': 'invalid'}, 'DNS server')]:
                        (directory / 'daemon.json').write_text(json.dumps({'name': 'old-daemon', **invalid}))
                        result = subprocess.run([str(BINARY), '--data-dir', str(directory), 'remote', 'list'], capture_output=True, text=True, timeout=10)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(result.stdout, '')
                        self.assertIn(message, result.stderr)
                    self.assertTrue(requests)
                    self.assertEqual(set(requests), {'status'})
                    self.assertEqual(rpc(directory, op='status')['version'], '0.0.0')
                finally:
                    stopping.set()
                    thread.join(timeout=2)

    def test_lazy_daemon_child_is_reaped_while_session_parent_is_alive(self):
        with tempfile.TemporaryDirectory(prefix='am-setup-',dir='/tmp') as root:
            root=Path(root);directory,config=root/'node',root/'home'
            result=self.invoke(directory,config,'--config-only')
            self.assertEqual(result.returncode,0,result.stderr)
            (directory/'daemon.json').write_text(json.dumps({'name':'reaping-test','bind_addr':'127.0.0.1:0','no_relay':True}))
            client=McpClient(directory,'parent')
            try:
                pid=rpc(directory,op='status')['pid']
                rpc(directory,op='shutdown')
                wait_for(lambda: subprocess.run(['ps','-p',str(pid),'-o','stat='],capture_output=True).returncode!=0)
                self.assertIsNone(client.process.poll())
            finally:
                client.close()
    def invoke(self, directory, config, *args):
        return subprocess.run([str(BINARY), '--data-dir', str(directory), 'setup', '--config-root', str(config), *args], capture_output=True, text=True, timeout=20)

    def test_shared_family_config_merge_is_idempotent_and_preserves_others(self):
        with tempfile.TemporaryDirectory(prefix='am-setup-', dir='/tmp') as root:
            root = Path(root)
            config, directory = root / 'home', root / 'node'
            config.mkdir()
            (config / '.codex').mkdir()
            (config / '.tclaude').mkdir()
            (config / '.tcodex').mkdir()
            (config / '.tclaude/.claude.json').write_text(json.dumps({'mcpServers':{'deepwiki':{'command':'existing-wrapper-tool'}},'theme':'wrapper-theme'}))
            (config/'.tclaude/settings.json').write_text(json.dumps({'crossSessionInbound':'hold','permissions':{'allow':['Read'],'deny':['Bash(*)']}}))
            (config / '.tcodex/config.toml').write_text('model_provider = "existing-wrapper-provider"\n[mcp_servers.existing]\ncommand = "wrapper-tool"\n')
            (config / '.claude.json').write_text(json.dumps({'theme': 'dark', 'mcpServers': {'existing': {'command': 'other-tool'}}}))
            (config / '.codex/config.toml').write_text('# preserve comment\nmodel = "existing-model"\n[mcp_servers.existing]\ncommand = "other-tool"\n[mcp_servers.agentmesh]\ncommand = '+json.dumps(str(directory/'bin/agentmesh'))+'\n[mcp_servers.agentmesh.tools.send_message]\napproval_mode = "prompt"\n')
            preview = self.invoke(directory, config, '--dry-run')
            self.assertEqual(preview.returncode, 0, preview.stderr)
            self.assertFalse(directory.exists())
            self.assertEqual(len(list(config.rglob('*backup*'))), 0)
            first = self.invoke(directory, config, '--config-only')
            self.assertEqual(first.returncode, 0, first.stderr)
            result = json.loads(first.stdout)
            self.assertEqual([p['harnesses'][0] for p in result['configurations']], ['claude','codex','tclaude','tcodex','inbound_policy','inbound_policy'])
            self.assertTrue((config/'.tclaude/.claude.json').exists())
            self.assertTrue((config/'.tcodex/config.toml').exists())
            self.assertIn('tcodex',(config/'.tcodex/config.toml').read_text())
            self.assertEqual(json.loads((config/'.tclaude/.claude.json').read_text())['mcpServers']['deepwiki']['command'],'existing-wrapper-tool')
            self.assertIn('model_provider = "existing-wrapper-provider"',(config/'.tcodex/config.toml').read_text())
            claude = json.loads((config / '.claude.json').read_text())
            self.assertEqual(claude['theme'], 'dark')
            self.assertEqual(claude['mcpServers']['existing']['command'], 'other-tool')
            self.assertEqual(claude['mcpServers']['agentmesh']['command'], str(directory / 'bin/agentmesh'))
            codex = (config / '.codex/config.toml').read_text()
            self.assertIn('# preserve comment', codex)
            self.assertIn('model = "existing-model"', codex)
            self.assertIn('[mcp_servers.existing]', codex)
            self.assertEqual(codex.count('[[hooks.SessionStart]]'),1)
            self.assertIn('trusted_hash', codex)
            self.assertIn('AGENTMESH_CODEX_HOME', codex)
            self.assertIn('approval_mode = "prompt"',codex)
            policy=json.loads((config/'.tclaude/settings.json').read_text())
            self.assertEqual(policy['crossSessionInbound'],'hold')
            self.assertEqual(policy['permissions']['deny'],['Bash(*)'])
            self.assertIn('Read',policy['permissions']['allow'])
            self.assertIn('mcp__agentmesh__check_inbox',policy['permissions']['allow'])
            self.assertEqual(json.loads((config/'.claude/settings.json').read_text())['crossSessionInbound'],'accept')
            second = self.invoke(directory, config, '--config-only')
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(json.loads(second.stdout)['backups'], [])

    def test_invalid_config_does_not_modify_the_other_family(self):
        with tempfile.TemporaryDirectory(prefix='am-setup-', dir='/tmp') as root:
            root = Path(root)
            config, directory = root / 'home', root / 'node'
            config.mkdir()
            (config / '.codex').mkdir()
            original = '{"theme":"light"}'
            (config / '.claude.json').write_text(original)
            (config / '.codex/config.toml').write_text('not valid = [')
            result = self.invoke(directory, config, '--config-only')
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual((config / '.claude.json').read_text(), original)
            self.assertFalse(directory.exists())

    def test_two_mcp_sessions_lazily_start_one_shared_daemon(self):
        with tempfile.TemporaryDirectory(prefix='am-setup-', dir='/tmp') as root:
            root = Path(root)
            directory, config = root / 'node', root / 'home'
            result = self.invoke(directory, config, '--config-only')
            self.assertEqual(result.returncode, 0, result.stderr)
            (directory / 'daemon.json').write_text(json.dumps({'name': 'setup-test', 'bind_addr': '127.0.0.1:0', 'no_relay': True}))
            clients = []
            try:
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    clients = list(pool.map(lambda name: McpClient(directory, name), ['first', 'second']))
                self.assertEqual(len(rpc(directory, op='directory')['sessions']), 2)
            finally:
                for client in clients:
                    client.close()
                if (directory / 'daemon.sock').exists():
                    rpc(directory, op='shutdown')
                    wait_for(lambda: not (directory / 'daemon.sock').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
