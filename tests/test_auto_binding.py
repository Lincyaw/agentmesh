import os
import unittest
import test_interop as interop
import uuid

from test_interop import BINARY, McpClient, rpc


class NativeBinding(unittest.TestCase):
    setUp=interop.Interoperability.setUp
    tearDown=interop.Interoperability.tearDown
    start=interop.Interoperability.start
    stop=interop.Interoperability.stop
    client=interop.Interoperability.client

    def test_all_tools_fence_the_native_thread_after_automatic_binding(self):
        node=self.start('A')
        client=McpClient(node['dir'],'bound',harness='codex',environment={**os.environ,'AGENTMESH_CODEX_BINARY':str(BINARY.resolve())})
        self.clients.append(client)
        thread=str(uuid.uuid4())
        identity=client.call('tools/call',{'name':'whoami','arguments':{},'_meta':{'threadId':thread}})
        self.assertFalse(identity.get('isError'))
        self.assertTrue(rpc(node['dir'],op='directory')['sessions'][0]['wake'])
        calls={'whoami':{},'check_inbox':{},'send_message':{'to':[client.session['address']],'body':'must not enqueue'},'acknowledge_messages':{'messages':[]},'set_group':{'name':'must-not-save','members':[client.session['address']]},'list_groups':{},'list_sessions':{}}
        calls.update({
            'remote_workspaces': {},
            'remote_list_files': {'node':'local','workspace':'test'},
            'remote_read_file': {'node':'local','workspace':'test','path':'test'},
            'remote_write_file': {'node':'local','workspace':'test','path':'test','content':'test'},
            'remote_search': {'node':'local','workspace':'test','query':'test'},
            'remote_execute': {'node':'local','workspace':'test','command':'true','request_id':'fenced'},
            'remote_job': {'node':'local','job':str(uuid.uuid4())},
            'remote_jobs': {'node':'local','workspace':'test'},
            'remote_cancel_job': {'node':'local','job':str(uuid.uuid4())},
        })
        for tool,args in calls.items():
            for metadata in [{},{'threadId':str(uuid.uuid4())}]:
                with self.subTest(tool=tool,metadata=bool(metadata)):
                    with self.assertRaisesRegex(RuntimeError,'another native thread'):
                        client.call('tools/call',{'name':tool,'arguments':args,'_meta':metadata})
        ok=client.call('tools/call',{'name':'check_inbox','arguments':{},'_meta':{'threadId':thread}})
        self.assertFalse(ok.get('isError'))
        self.assertEqual(rpc(node['dir'],op='deliveries'),[])


if __name__=='__main__':unittest.main(verbosity=2)
