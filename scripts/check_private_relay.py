"""Run interoperability through an isolated, token-protected, locally hosted iroh relay."""
import argparse
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--relay-binary', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/agentmesh'))
    parser.add_argument('--bootstrap-only', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    relay_binary = args.relay_binary.resolve()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='am-private-relay-', dir='/tmp') as temporary:
        directory = Path(temporary)
        token = secrets.token_urlsafe(32)
        token_file = directory / 'token'
        token_file.write_text(token)
        token_file.chmod(0o600)
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as reservation:
            reservation.bind(('127.0.0.1', 0))
            port = reservation.getsockname()[1]
        config = directory / 'relay.toml'
        config.write_text(f'http_bind_addr = "127.0.0.1:{port}"\nenable_metrics = false\nenable_quic_addr_discovery = false\naccess.shared_token = [{json.dumps(token)}]\n')
        config.chmod(0o600)
        relay_environment = {**os.environ, 'RUST_LOG': 'warn'}
        relay_environment.pop('IROH_RELAY_ACCESS_TOKEN', None)
        with (directory / 'relay.log').open('w') as log:
            server = subprocess.Popen([str(relay_binary), '--dev', '--config-path', str(config)], stdout=log, stderr=log,
                                      env=relay_environment)
            try:
                deadline = time.monotonic() + 15
                while True:
                    if server.poll() is not None:
                        raise RuntimeError('private relay exited before becoming ready')
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=0.2):
                            break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise TimeoutError('private relay did not listen')
                        time.sleep(0.05)
                environment = {**os.environ, 'AGENTMESH_BINARY': str(binary),
                               'AGENTMESH_RELAY_URL': f'http://127.0.0.1:{port}',
                               'AGENTMESH_RELAY_TOKEN_FILE': str(token_file), 'AGENTMESH_RELAY_ONLY': '1'}
                wrong_token = directory / 'wrong-token'
                wrong_token.write_text(secrets.token_urlsafe(32))
                wrong_token.chmod(0o600)
                denied_dir = directory / 'denied'
                with (directory / 'denied.log').open('w') as denied_log:
                    denied = subprocess.Popen([str(binary), '--data-dir', str(denied_dir), 'daemon', 'run', '--name', 'denied',
                                               '--relay-url', environment['AGENTMESH_RELAY_URL'], '--relay-only',
                                               '--relay-token-file', str(wrong_token)], stdout=denied_log, stderr=denied_log)
                    try:
                        deadline = time.monotonic() + 10
                        while not (denied_dir / 'daemon.sock').exists():
                            if denied.poll() is not None or time.monotonic() >= deadline:
                                raise RuntimeError('negative-auth endpoint did not start')
                            time.sleep(0.05)
                        rejected = subprocess.run([str(binary), '--data-dir', str(denied_dir), 'relay', 'check'], capture_output=True, text=True, timeout=20)
                        if rejected.returncode == 0:
                            raise AssertionError('private relay accepted an incorrect token')
                    finally:
                        denied.terminate()
                        denied.wait(timeout=10)
                suites = ['test_bootstrap.py'] if args.bootstrap_only else ['test_invites.py', 'test_remote.py', 'test_interop.py', 'test_bootstrap.py']
                for suite in suites:
                    subprocess.run(['python3', str(root / 'tests' / suite)], cwd=root,
                                   env=environment, check=True, timeout=240)
                print(json.dumps({'private_relay': True, 'direct_transports': False,
                                  'token_auth': True, 'wrong_token_rejected': True, 'suites': len(suites), 'checks': 6 if args.bootstrap_only else 25, 'status': 'passed'}))
            finally:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == '__main__':
    main()
