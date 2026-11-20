import base64
import json
import subprocess
import time
import sys
import unittest
import test_interop as interop

from test_interop import BINARY, rpc, wait_for


class InvitationNetwork(unittest.TestCase):
    setUp=interop.Interoperability.setUp
    tearDown=interop.Interoperability.tearDown
    start=interop.Interoperability.start
    stop=interop.Interoperability.stop
    client=interop.Interoperability.client
    def test_invitation_forms_mesh_and_works_without_original_inviter(self):
        a,b,c,d=[self.start(name) for name in ['A','B','C','D']]
        first=rpc(a['dir'],op='invite_create',ttl_seconds=600)['code']
        self.assertTrue(rpc(b['dir'],op='join',code=first)['joined'])
        self.assertTrue(rpc(b['dir'],op='join',code=first)['joined'])
        with self.assertRaises(RuntimeError):rpc(c['dir'],op='join',code=first)
        second=rpc(a['dir'],op='invite_create',ttl_seconds=600)['code']
        rpc(c['dir'],op='join',code=second)
        wait_for(lambda: len(rpc(b['dir'],op='peers'))==2 and len(rpc(c['dir'],op='peers'))==2,timeout=25)
        sender=self.client(b,'sender');receiver=self.client(c,'receiver')
        self.stop(a)
        third=rpc(b['dir'],op='invite_create',ttl_seconds=600)['code']
        rpc(d['dir'],op='join',code=third)
        wait_for(lambda: len(rpc(c['dir'],op='peers'))==3,timeout=25)
        sent=sender.tool('send_message',to=[receiver.session['address']],body='mesh without enrollment seed')
        self.assertEqual(wait_for(receiver.inbox)[0]['message']['id'],sent['id'])
        # Joining through the CLI also installs all four harness entries in one operation.
        e=self.start('E');invite=rpc(c['dir'],op='invite_create',ttl_seconds=600)['code']
        result=subprocess.run([str(BINARY),'--data-dir',str(e['dir']),'join','--config-root',str(self.root/'new-home')],input=invite+'\n',text=True,capture_output=True,timeout=25)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue(json.loads(result.stdout)['joined'])
        self.assertTrue((self.root/'new-home/.tcodex/config.toml').exists())

    def test_expired_tampered_and_wrong_network_invites_are_rejected(self):
        a,b,c=[self.start(name) for name in ['A','B','C']]
        code=rpc(a['dir'],op='invite_create',ttl_seconds=1)['code']
        time.sleep(1.1)
        with self.assertRaises(RuntimeError):rpc(b['dir'],op='join',code=code)
        code=rpc(a['dir'],op='invite_create',ttl_seconds=600)['code']
        raw=json.loads(base64.urlsafe_b64decode(code[4:]+'='*(-len(code[4:])%4)))
        raw['secret']='A'*43
        modified='am1_'+base64.urlsafe_b64encode(json.dumps(raw).encode()).decode().rstrip('=')
        with self.assertRaises(RuntimeError):rpc(b['dir'],op='join',code=modified)
        self.assertEqual(rpc(a['dir'],op='peers'),[])
        rpc(c['dir'],op='invite_create',ttl_seconds=600)
        with self.assertRaises(RuntimeError):rpc(c['dir'],op='join',code=code)
        self.assertEqual(rpc(c['dir'],op='peers'),[])


if __name__=='__main__':
    unittest.main(verbosity=2)
