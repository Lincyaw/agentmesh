import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
import uuid

from test_interop import BINARY, Interoperability, rpc, wait_for


def main():
    binary = os.environ.get('CLAUDE_BINARY') or shutil.which('claude')
    if not binary:
        raise RuntimeError('Set CLAUDE_BINARY to an authenticated Claude Code executable or wrapper')
    case = Interoperability()
    case.setUp()
    process = None
    log = None
    events = []
    try:
        a, b = case.start('A'), case.start('B')
        case.pair(a, b)
        sender = case.client(a, 'model-test-sender')
        workspace = case.root / 'workspace'
        workspace.mkdir(mode=0o700)
        session = str(uuid.uuid4())
        config = case.root / 'mcp.json'
        config.write_text(json.dumps({'mcpServers': {'agentmesh': {'command': str(BINARY), 'args': ['--data-dir', str(b['dir']), 'mcp', 'serve', '--harness', 'claude', '--session', session, '--name', 'live-claude-model']}}}))
        config_root=case.root/'setup-home'
        subprocess.run([str(BINARY),'--data-dir',str(b['dir']),'setup','--config-root',str(config_root),'--config-only'],check=True,capture_output=True,timeout=20)
        settings=json.loads((config_root/'.tclaude/settings.json').read_text())
        environment = os.environ.copy()
        environment['CLAUDE_CONFIG_DIR'] = str(case.root / 'claude-config')
        command = [binary, '-p', '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose', '--setting-sources', '', '--settings', json.dumps(settings), '--strict-mcp-config', '--mcp-config', str(config), '--tools', '', '--disable-slash-commands', '--no-session-persistence', '--max-budget-usd', '0.50']
        log = open(case.root / 'claude.log', 'w')
        process = subprocess.Popen(command, cwd=workspace, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)

        def collect():
            for line in process.stdout:
                try:
                    events.append(json.loads(line))
                except json.JSONDecodeError:
                    events.append({'type': 'invalid_output'})
        reader = threading.Thread(target=collect, daemon=True)
        reader.start()
        initial = 'Reply exactly AGENTMESH_READY. On later Agentmesh inbox reminders, read the pending messages with check_inbox. For validation messages, send a reply using send_message to the original from address, preserve conversation, set reply_to to the original message id, copy its body exactly as your reply, then acknowledge it. Only use Agentmesh tools. Do not reply to response messages or start any other work.'
        process.stdin.write(json.dumps({'type': 'user', 'message': {'role': 'user', 'content': initial}}).encode() + b'\n')
        process.stdin.flush()
        print(json.dumps({'phase':'waiting_for_model_ready'}),file=sys.stderr,flush=True)
        ready = wait_for(lambda: next((e for e in events if e.get('type') == 'result'), None), timeout=180)
        assert not ready.get('is_error'), {'subtype': ready.get('subtype'), 'result': ready.get('result')}
        assert 'AGENTMESH_READY' in ready.get('result', ''), ready.get('result')
        print(json.dumps({'phase':'model_ready'}),file=sys.stderr,flush=True)
        receiver = next(s for s in rpc(b['dir'], op='directory')['sessions'] if s['id'] == session)
        assert receiver['wake'], 'native binding was not automatic'
        nonce = 'AGENTMESH_LIVE_' + uuid.uuid4().hex
        sent = sender.tool('send_message', to=[receiver['address']], body=nonce)
        print(json.dumps({'phase':'waiting_for_native_reply'}),file=sys.stderr,flush=True)
        replies = wait_for(sender.inbox, timeout=120)
        reply = next(item['message'] for item in replies if item['message'].get('reply_to') == sent['id'])
        assert reply['body'] == nonce
        assert reply['conversation'] == sent['conversation']
        assert reply['from'] == receiver['address']
        def acknowledged():
            with sqlite3.connect(b['dir'] / 'mailbox.sqlite') as database:
                row=database.execute('SELECT ack FROM inbox WHERE id=?',(sent['id'],)).fetchone()
            return row is not None and row[0] == 1
        assert wait_for(acknowledged,timeout=30)
        tool_names = sorted({item.get('name') for event in events if event.get('type') == 'assistant' for item in event.get('message', {}).get('content', []) if item.get('type') == 'tool_use'})
        assert any(name.endswith('check_inbox') for name in tool_names)
        assert any(name.endswith('send_message') for name in tool_names)
        print(json.dumps({'executable': binary, 'version': subprocess.check_output([binary, '--version'], text=True).strip(), 'automatic_binding': True, 'native_idle_wake': True, 'model_read_and_reply': True, 'model_acknowledged': True, 'cross_node_reply_verified': True, 'tools_called': tool_names}))
    finally:
        if process:
            if process.poll() is None:
                process.stdin.close()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(timeout=10)
            process.stdout.close()
        if log:
            log.close()
        case.tearDown()


if __name__ == '__main__':
    main()
