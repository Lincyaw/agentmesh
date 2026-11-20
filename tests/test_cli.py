import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_interop import BINARY


class CliContract(unittest.TestCase):
    def test_relay_token_rejects_public_or_multiline_files_without_disclosure(self):
        with tempfile.TemporaryDirectory(prefix='am-cli-',dir='/tmp') as root:
            root=Path(root)
            token=root/'token';secret='fixture-relay-credential'
            token.write_text(secret);token.chmod(0o644)
            command=[str(BINARY),'--data-dir',str(root/'node'),'daemon','run','--name','test','--relay-url','https://relay.invalid','--relay-token-file',str(token)]
            for content,permissions in [(secret,0o644),(secret+'\nsecond-line',0o600)]:
                token.write_text(content);token.chmod(permissions)
                result=subprocess.run(command,capture_output=True,text=True,timeout=5)
                self.assertNotEqual(result.returncode,0)
                self.assertNotIn(secret,result.stdout+result.stderr)
    def test_all_help_paths_complete_without_starting_a_daemon(self):
        commands = [[], ['invite'], ['invite', 'create'], ['join'], ['setup'], ['daemon'], ['daemon', 'run'], ['daemon', 'stop'], ['peer'], ['peer', 'ticket'], ['peer', 'add'], ['peer', 'list'], ['session'], ['session', 'list'], ['session', 'attach'], ['mcp'], ['mcp', 'serve'], ['message'], ['message', 'send'], ['message', 'inbox'], ['message', 'ack'], ['message', 'deliveries'], ['group'], ['group', 'set'], ['group', 'list'], ['config'], ['status'], ['relay'], ['relay', 'check']]
        with tempfile.TemporaryDirectory(prefix='am-cli-', dir='/tmp') as root:
            directory = Path(root) / 'uncreated'
            for command in commands:
                with self.subTest(command=command):
                    result = subprocess.run([str(BINARY), '--data-dir', str(directory), *command, '--help'], capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn('Usage:', result.stdout)
            self.assertFalse(directory.exists())

    def test_configuration_is_readonly_and_errors_stay_on_stderr(self):
        with tempfile.TemporaryDirectory(prefix='am-cli-', dir='/tmp') as root:
            directory = Path(root) / 'uncreated'
            result = subprocess.run([str(BINARY), '--data-dir', str(directory), 'config', '--harness', 'claude'], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(json.loads(result.stdout)['mcpServers']['agentmesh']['command'], str(BINARY.resolve()))
            self.assertFalse(directory.exists())
            error = subprocess.run([str(BINARY), '--data-dir', str(directory), 'status'], capture_output=True, text=True, timeout=5)
            self.assertNotEqual(error.returncode, 0)
            self.assertEqual(error.stdout, '')
            self.assertFalse(json.loads(error.stderr)['ok'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
