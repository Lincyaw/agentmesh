import fcntl
import json
import os
from pathlib import Path
import pty
import shutil
import signal
import select
import sqlite3
import struct
import subprocess
import sys
import termios
import threading
import uuid

from live_codex import prepare_profile
from test_interop import Interoperability, rpc, wait_for


class Terminal:
    def __init__(self, command, environment):
        self.master,slave=pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,160,0,0))
        self.output=bytearray()
        self.stopping=threading.Event()
        self.process=subprocess.Popen(command,env={**environment,'TERM':'xterm-256color'},stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        self.reader=threading.Thread(target=self.collect,daemon=True);self.reader.start()
    def collect(self):
        try:
            while not self.stopping.is_set():
                if not select.select([self.master],[],[],0.2)[0]:continue
                data=os.read(self.master,65536)
                if not data:return
                self.output.extend(data)
                if b'\x1b[6n' in data:os.write(self.master,b'\x1b[1;1R')
                if b'\x1b[c' in data:os.write(self.master,b'\x1b[?1;2c')
        except OSError:pass
    def close(self):
        self.stopping.set()
        if self.process.poll() is None:
            os.killpg(self.process.pid,signal.SIGTERM)
            try:self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid,signal.SIGKILL);self.process.wait()
        self.reader.join(timeout=2)
        os.close(self.master)


def main():
    binary=os.environ.get('CODEX_BINARY') or shutil.which('codex')
    if not binary:raise RuntimeError('Set CODEX_BINARY to an authenticated Codex executable')
    case=Interoperability();case.setUp();terminals=[];environment=None
    try:
        a,b=case.start('A'),case.start('B');case.pair(a,b)
        sender=case.client(a,'model-test-sender')
        environment=prepare_profile(binary,case.root/'codex-home',b['dir'])
        home=Path(environment['CODEX_HOME']);workspace=case.root/'workspace';workspace.mkdir()
        with (home/'config.toml').open('a') as config:
            config.write('\n[projects.'+json.dumps(str(workspace))+']\ntrust_level="trusted"\n')
        initial='Reply exactly AGENTMESH_READY. On later Agentmesh inbox reminders, use check_inbox to read pending messages, send a reply to their original from address, preserve conversation, set reply_to to the original id, copy the body exactly as the reply, then acknowledge it. If Agentmesh tools are deferred, discover them with native tool search first. Then only call Agentmesh tools; do not run shell commands or start other work.'
        command=[binary,'--no-alt-screen','-C',str(workspace),'-a','never','-s','workspace-write']
        if os.environ.get('CODEX_MODEL'):command.extend(['-m',os.environ['CODEX_MODEL']])
        for _ in range(2):terminals.append(Terminal([*command,initial],environment))
        def ready():
            for terminal in terminals:
                assert terminal.process.poll() is None, f"CLI exited before ready (code {terminal.process.returncode})"
            count=0
            for path in (home/'sessions').rglob('*.jsonl'):
                lines=path.read_text(errors='replace').splitlines()
                for line in lines:
                    try:event=json.loads(line)
                    except json.JSONDecodeError:continue
                    payload=event.get('payload',{})
                    if event.get('type')=='event_msg' and payload.get('type')=='task_complete' and payload.get('error'):
                        error=payload['error']
                        info=error.get('codex_error_info',{})
                        raise AssertionError('native model backend failed: '+str(info))
                    if event.get('type')=='event_msg' and payload.get('type')=='task_complete' and 'AGENTMESH_READY' in (payload.get('last_agent_message') or ''):
                        count+=1;break
            return count>=2
        print(json.dumps({'phase':'waiting_for_two_cli_models_ready'}),file=sys.stderr,flush=True)
        wait_for(ready,timeout=180)
        with sqlite3.connect(b['dir']/'mailbox.sqlite') as db:
            bindings={json.loads(wake)['thread']:sid for sid,wake in db.execute('SELECT id,wake FROM sessions WHERE wake IS NOT NULL')}
        assert len(bindings)==2 and len(set(bindings.values()))==2
        sessions=rpc(b['dir'],op='directory')['sessions']
        target=next(s for s in sessions if s['id'] in bindings.values())
        other=next(s for s in sessions if s['id']!=target['id'] and s['id'] in bindings.values())
        nonce='AGENTMESH_CODEX_AUTO_'+uuid.uuid4().hex
        sent=sender.tool('send_message',to=[target['address']],body=nonce)
        print(json.dumps({'phase':'waiting_for_native_model_reply'}),file=sys.stderr,flush=True)
        response=wait_for(lambda: next((item['message'] for item in sender.inbox() if item['message'].get('reply_to')==sent['id']),None),timeout=180)
        assert response['body']==nonce and response['from']==target['address'] and response['conversation']==sent['conversation']
        def acknowledged():
            with sqlite3.connect(b['dir']/'mailbox.sqlite') as db:
                return db.execute('SELECT ack FROM inbox WHERE id=?',(sent['id'],)).fetchone()[0]==1
        wait_for(acknowledged,timeout=30)
        with sqlite3.connect(b['dir']/'mailbox.sqlite') as db:
            assert db.execute('SELECT count(*) FROM inbox WHERE recipient=?',(other['id'],)).fetchone()[0]==0
        print(json.dumps({'automatic_binding':True,'two_cli_sessions_isolated':True,'native_idle_wake':True,'model_read_reply_ack':True,'cross_node_reply_verified':True}))
    finally:
        for terminal in terminals:terminal.close()
        if environment:subprocess.run([binary,'app-server','daemon','stop'],env=environment,capture_output=True,timeout=20)
        case.tearDown()


if __name__=='__main__':main()
