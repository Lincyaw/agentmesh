"""Package an already-built target binary and its public documentation."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--platform', required=True, choices=['macos-arm64', 'linux-x64', 'linux-arm64'])
    parser.add_argument('--output', type=Path, default=Path('dist/release'))
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    metadata = json.loads(subprocess.check_output([os.environ.get('CARGO', 'cargo'), 'metadata', '--format-version', '1', '--no-deps', '--offline'], cwd=root, text=True))
    version = next(package['version'] for package in metadata['packages'] if package['name'] == 'agentmesh')
    arguments.output.mkdir(parents=True, exist_ok=True)
    name = f'agentmesh-v{version}-{arguments.platform}'
    archive = arguments.output / f'{name}.tar.gz'
    with tarfile.open(archive, 'w:gz') as output:
        output.add(arguments.binary, arcname=f'{name}/agentmesh')
        for path in ['README.md', 'LICENSE', 'docs/architecture.md', 'docs/validation.md', 'docs/remote-workspaces.md', 'docs/onboarding.md']:
            output.add(root / path, arcname=f'{name}/{path}')
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (arguments.output / f'{archive.name}.sha256').write_text(f'{digest}  {archive.name}\n')
    print(archive)


if __name__ == '__main__':
    main()
