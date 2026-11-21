import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import tarfile
import tempfile
import time
import unittest

import test_interop as fixtures
from test_interop import BINARY, ROOT, rpc, wait_for


def encode(value):
    return 'amb1_' + base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip('=')


class Bootstrap(unittest.TestCase):
    def setUp(self):
        fixtures.Interoperability.setUp(self)
        self.joined = []

    def tearDown(self):
        for directory in self.joined:
            if (directory/'daemon.sock').exists():
                rpc(directory,op='shutdown')
                wait_for(lambda: not (directory/'daemon.sock').exists())
        fixtures.Interoperability.tearDown(self)
    start = fixtures.Interoperability.start
    stop = fixtures.Interoperability.stop
    client = fixtures.Interoperability.client

    def fixture(self):
        return {'version': 1, 'invitation': {'version': 1, 'inviter': {'version': 1, 'name': 'fixture', 'address': {'id': '58'+'66'*31, 'addrs': [{'Relay': 'http://127.0.0.1:9/'}]}}, 'secret': base64.urlsafe_b64encode(b'a'*32).decode().rstrip('='), 'expires_at': int(time.time()*1000)+60000}, 'relay': {'url': 'http://127.0.0.1:9/', 'token': None, 'resolve': None}}

    def join(self, directory, home, code):
        return subprocess.run([str(BINARY), '--data-dir', str(directory), 'join', '--config-root', str(home), '--no-service'], input=code+'\n', capture_output=True, text=True, timeout=35)

    def test_invalid_bundle_makes_no_installation(self):
        directory, home = self.root/'new', self.root/'home'
        invalid = self.fixture()
        invalid['invitation']['expires_at'] = 1
        result = self.join(directory, home, encode(invalid))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(directory.exists())
        self.assertFalse(home.exists())
        invalid = self.fixture()
        invalid['relay']['url'] = 'https://other-relay.invalid/'
        result = self.join(directory, home, encode(invalid))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(directory.exists())

    def test_existing_relay_configuration_is_preserved(self):
        directory, home = self.root/'existing', self.root/'home'
        directory.mkdir()
        original = json.dumps({'name':'existing','relay_url':'https://existing-relay.invalid/'})
        (directory/'daemon.json').write_text(original)
        result = self.join(directory, home, encode(self.fixture()))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('different relay', result.stderr)
        self.assertEqual((directory/'daemon.json').read_text(), original)
        self.assertFalse(home.exists())

    def issuer(self):
        if not os.environ.get('AGENTMESH_RELAY_URL'):
            self.skipTest('requires the private-relay runner')
        issuer = self.start('issuer')
        profile = {'name':'issuer','relay_url':os.environ['AGENTMESH_RELAY_URL'],'relay_only':True}
        if os.environ.get('AGENTMESH_RELAY_TOKEN_FILE'):
            profile['relay_token_file'] = os.environ['AGENTMESH_RELAY_TOKEN_FILE']
        (issuer['dir']/'daemon.json').write_text(json.dumps(profile))
        result = subprocess.run([str(BINARY),'--data-dir',str(issuer['dir']),'invite','create','--bootstrap'], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode,0,result.stderr)
        code = json.loads(result.stdout)['code']
        return issuer, code

    def track_joined(self, directory):
        self.joined.append(directory)

    def test_join_configures_four_harnesses_and_controller_workspace(self):
        issuer,code = self.issuer()
        directory,home = self.root/'joined',self.root/'new-home'
        result = self.join(directory,home,code)
        self.assertEqual(result.returncode,0,result.stderr)
        self.track_joined(directory)
        value = json.loads(result.stdout)
        self.assertEqual(value['workspace']['controller'],issuer['identity'])
        for file in ['.claude.json','.tclaude/.claude.json','.codex/config.toml','.tcodex/config.toml']:
            self.assertTrue((home/file).exists())
        self.assertEqual((directory/'relay-token').stat().st_mode & 0o777,0o600)
        peer = rpc(directory,op='status')['node']
        controller = self.client(issuer,'controller')
        controller.tool('remote_write_file',node=peer,workspace='workspace',path='proof',content='bootstrap works')
        self.assertEqual((home/'agentmesh-workspace/proof').read_text(),'bootstrap works')
        again = self.join(directory,home,code)
        self.assertEqual(again.returncode,0,again.stderr)
        rejected = self.join(self.root/'other',self.root/'other-home',code)
        self.assertNotEqual(rejected.returncode,0)
        other = self.root/'other'
        self.track_joined(other)
        self.assertFalse((self.root/'other-home/agentmesh-workspace').exists())

    def distribution(self, bad=False):
        assets = self.root/'assets'; assets.mkdir()
        target = {'Linux':'linux','Darwin':'macos'}[platform.system()]
        arch = 'arm64' if platform.machine() in ['arm64','aarch64'] else 'x64'
        name = f'agentmesh-v0.5.0-{target}-{arch}'
        archive = assets/(name+'.tar.gz')
        with tarfile.open(archive,'w:gz') as output:
            output.add(BINARY,arcname=name+'/agentmesh')
        digest = '0'*64 if bad else hashlib.sha256(archive.read_bytes()).hexdigest()
        (assets/'SHA256SUMS').write_text(digest+'  '+archive.name+'\n')
        tools = self.root/'tools';tools.mkdir()
        curl = tools/'curl'
        curl.write_text('#!/usr/bin/env python3\nimport os,pathlib,shutil,sys\na=sys.argv[1:];url=next(x for x in a if x.startswith("https://"));out=a[a.index("-o")+1];shutil.copyfile(pathlib.Path(os.environ["TEST_ASSETS"])/url.rsplit("/",1)[1],out)\n')
        curl.chmod(0o700)
        return {**os.environ,'PATH':str(tools)+os.pathsep+os.environ['PATH'],'TEST_ASSETS':str(assets),'HOME':str(self.root/'install-home'),'AGENTMESH_DATA_DIR':str(self.root/'installed'),'AGENTMESH_DOWNLOAD_BASE':'https://fixture.invalid'}

    def test_installer_download_join_and_rerun(self):
        issuer,code = self.issuer()
        env = self.distribution()
        env['AGENTMESH_INVITE_CODE'] = code
        command = ['sh',str(ROOT/'scripts/install.sh'),'--join','--no-service']
        result = subprocess.run(command,env=env,capture_output=True,text=True,timeout=40)
        self.assertEqual(result.returncode,0,result.stderr)
        directory = self.root/'installed'
        self.track_joined(directory)
        self.assertTrue((Path(env['HOME'])/'.local/bin/agentmesh').is_file())
        self.assertTrue(json.loads(result.stdout)['joined'])
        again = subprocess.run(command,env=env,capture_output=True,text=True,timeout=40)
        self.assertEqual(again.returncode,0,again.stderr)

    def test_bad_download_checksum_installs_nothing(self):
        env = self.distribution(bad=True)
        result = subprocess.run(['sh',str(ROOT/'scripts/install.sh')],env=env,capture_output=True,text=True,timeout=20)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('verification failed',result.stderr)
        self.assertFalse((Path(env['HOME'])/'.local/bin/agentmesh').exists())
        self.assertFalse((Path(env['HOME'])/'.claude.json').exists())
        self.assertFalse(Path(env['AGENTMESH_DATA_DIR']).exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
