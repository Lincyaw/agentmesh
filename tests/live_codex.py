import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import time

from test_interop import BINARY, Interoperability, rpc, wait_for


def prepare_profile(binary, directory, node_directory):
    environment = os.environ.copy()
    directory.mkdir(mode=0o700)
    harness = 'tcodex' if 'tcodex' in Path(binary).name else 'codex'
    variable = 'AGENTMESH_TCODEX_BINARY' if harness == 'tcodex' else 'AGENTMESH_CODEX_BINARY'
    install_env = {**os.environ, variable: binary}
    subprocess.run([str(BINARY), '--data-dir', str(node_directory), 'setup', '--config-root', str(directory), '--config-only'], env=install_env, check=True, capture_output=True, timeout=20)
    home = directory / ('.tcodex' if harness == 'tcodex' else '.codex')
    environment['CODEX_HOME'] = str(home)
    auth = Path.home() / '.codex/auth.json'
    if auth.exists():
        shutil.copy2(auth, home / 'auth.json')
    return environment


class AppServer:
    def __init__(self, binary, directory, node_directory):
        self.environment = prepare_profile(binary, directory, node_directory)
        self.binary = binary
        self.log = open(directory / 'app-server.log', 'w')
        self.process = subprocess.Popen([binary, 'app-server', '--stdio'], env=self.environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, bufsize=0)
        self.sequence = 0
        self.call('initialize', {'clientInfo': {'name': 'agentmesh-validation', 'version': '0.1.0'}, 'capabilities': {'experimentalApi': True}})
        self.notify('initialized')

    def notify(self, method):
        self.process.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method}).encode() + b'\n')
        self.process.stdin.flush()

    def call(self, method, params):
        self.sequence += 1
        self.process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params}).encode() + b'\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if select.select([self.process.stdout], [], [], 1)[0]:
                line = self.process.stdout.readline()
                if not line:
                    raise RuntimeError('Codex app-server exited')
                response = json.loads(line)
                if response.get('id') == self.sequence:
                    if 'error' in response:
                        raise RuntimeError(response['error'])
                    return response['result']
        raise RuntimeError(f'Codex app-server method timed out: {method}')

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=5)
        subprocess.run([self.binary, 'app-server', 'daemon', 'stop'], env=self.environment, capture_output=True, timeout=15)
        self.log.close()


def main():
    binary = os.environ.get('CODEX_BINARY') or shutil.which('codex')
    if not binary:
        raise RuntimeError('Codex CLI is not installed')
    case = Interoperability()
    case.setUp()
    app = None
    try:
        a, b = case.start('A'), case.start('B')
        case.pair(a, b)
        sender = case.client(a, 'external-sender')
        app = AppServer(binary, case.root / 'codex-home', b['dir'])
        workspace = case.root / 'workspace'
        workspace.mkdir()
        thread = app.call('thread/start', {'cwd': str(workspace), 'approvalPolicy': 'never', 'sandbox': 'workspace-write'})['thread']['id']
        def tools_ready():
            status = app.call('mcpServerStatus/list', {'threadId': thread, 'serverName': 'agentmesh'})
            return status if any(server.get('tools') for server in status.get('data', [])) else None
        status = wait_for(tools_ready, timeout=30)
        tools = next(server['tools'] for server in status['data'] if server.get('name') == 'agentmesh')
        hooks = app.call('hooks/list', {'threadId': thread})
        assert any(h.get('server') == 'agentmesh' and h.get('trustStatus') == 'trusted' for item in hooks['data'] for h in item['hooks'])
        app.call('turn/start', {'threadId': thread, 'input': [{'type': 'text', 'text': 'Reply only READY. Do not use tools.'}]})
        wait_for(lambda: any(s['wake'] for s in rpc(b['dir'], op='directory')['sessions']), timeout=30)
        identity = app.call('mcpServer/tool/call', {'threadId': thread, 'server': 'agentmesh', 'tool': 'whoami', 'arguments': {}})
        result = identity.get('result', identity)
        session = json.loads(result['content'][0]['text'])['session']
        assert session['wake'], 'SessionStart hook did not automatically bind the native thread'
        message = sender.tool('send_message', to=[session['address']], body='live Codex MCP interoperability')
        def read():
            response = app.call('mcpServer/tool/call', {'threadId': thread, 'server': 'agentmesh', 'tool': 'check_inbox', 'arguments': {}})
            result = response.get('result', response)
            return json.loads(result['content'][0]['text'])
        received = wait_for(read)
        assert received[0]['message']['id'] == message['id']
        reply = app.call('mcpServer/tool/call', {'threadId': thread, 'server': 'agentmesh', 'tool': 'send_message', 'arguments': {'to': [sender.session['address']], 'body': 'Codex MCP reply', 'conversation': message['conversation'], 'reply_to': message['id']}})
        assert not reply.get('isError')
        assert wait_for(sender.inbox)[0]['message']['body'] == 'Codex MCP reply'
        print(json.dumps({'phase':'mcp_interop_passed','cross_node_receive':True,'cross_node_reply':True,'mcp_tools':len(tools)}),file=sys.stderr,flush=True)
        queued = subprocess.run([binary, 'queue', '--thread', thread, '--message', 'Agentmesh validation: inbox reminder'], env=app.environment, capture_output=True, text=True, timeout=20)
        assert queued.returncode == 0, queued.stderr
        print(json.dumps({'codex_version': subprocess.check_output([binary, '--version'], text=True).strip(), 'thread': thread, 'mcp_tools': len(tools), 'cross_node_receive': True, 'cross_node_reply': True, 'queue_accepted': True, 'automatic_binding': True, 'model_execution_tested': False}))
    finally:
        if app:
            app.close()
        case.tearDown()


if __name__ == '__main__':
    main()
