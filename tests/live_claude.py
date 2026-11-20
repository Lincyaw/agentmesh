import json
import os
from pathlib import Path
import shutil
import subprocess

from test_interop import BINARY, Interoperability


def main():
    binary = os.environ.get('CLAUDE_BINARY') or shutil.which('claude')
    if not binary:
        candidates = sorted((Path.home() / '.vscode/extensions').glob('anthropic.claude-code-*/resources/native-binary/claude'))
        if candidates:
            binary = str(candidates[-1])
    if not binary:
        raise RuntimeError('Set CLAUDE_BINARY to an installed official Claude Code executable')
    case = Interoperability()
    case.setUp()
    try:
        node = case.start('A')
        environment = os.environ.copy()
        environment['CLAUDE_CONFIG_DIR'] = str(case.root / 'claude-config')
        entry = {'command': str(BINARY), 'args': ['--data-dir', str(node['dir']), 'mcp', 'serve', '--harness', 'claude', '--name', 'real-claude-healthcheck']}
        add = subprocess.run([binary, 'mcp', 'add-json', '--scope', 'user', 'agentmesh', json.dumps(entry)], env=environment, cwd=case.root, capture_output=True, text=True, timeout=20)
        assert add.returncode == 0, add.stderr
        check = subprocess.run([binary, 'mcp', 'list'], env=environment, cwd=case.root, capture_output=True, text=True, timeout=30)
        assert check.returncode == 0 and 'Connected' in check.stdout, check.stdout + check.stderr
        print(json.dumps({'claude_version': subprocess.check_output([binary, '--version'], text=True).strip(), 'mcp_connected': True, 'model_execution_tested': False}))
    finally:
        case.tearDown()


if __name__ == '__main__':
    main()
