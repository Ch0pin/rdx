"""Package a native release build, including runtime instructions and notices."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

from package_macos import HELPER, package_app, resolve_helper

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--platform', required=True, choices=['macos-arm64', 'macos-x86_64', 'windows-x86_64', 'linux-x86_64'])
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--helper', type=Path, help='Matching release helper (required for macOS/Linux)')
    args = parser.parse_args()
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    if args.tag != 'v' + version:
        raise SystemExit(f'Tag {args.tag} does not match Cargo version {version}')
    if args.platform.startswith('windows-') and args.helper is not None:
        raise SystemExit('Windows fallback is disabled; do not supply --helper.')
    helper = None if args.platform.startswith('windows-') else resolve_helper(ROOT, args.helper)
    name = f'rdx-{args.tag}-{args.platform}'
    staging_root = ROOT / 'target/release-packages'
    staging_root.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(dir=staging_root))
    stage = temporary / name
    stage.mkdir()
    binary_dir = args.binary_dir.resolve()
    if args.platform.startswith('macos-'):
        package_app(binary_dir, stage / 'RDX.app', helper, root=ROOT)
    else:
        suffix = '.exe' if args.platform.startswith('windows-') else ''
        for executable in ['rdx', 'rdx-source-stats']:
            shutil.copy2(binary_dir / (executable + suffix), stage / (executable + suffix))
        if helper is not None:
            shutil.copy2(helper, stage / HELPER)
        shutil.copytree(ROOT / 'third_party', stage / 'third_party')
        shutil.copytree(ROOT / 'assets/icons', stage / 'icons')
    for notice in ('LICENSE', 'NOTICE'):
        shutil.copy2(ROOT / notice, stage / notice)
    shutil.copy2(ROOT / 'docs/install.md', stage / 'START-HERE.md')
    shutil.copytree(ROOT / 'docs', stage / 'docs', ignore=shutil.ignore_patterns('images'))
    shutil.copytree(ROOT / 'plugins', stage / 'plugins', ignore=shutil.ignore_patterns('__pycache__', '*.pyc', '.DS_Store', 'tests'))
    (stage / 'VERSION').write_text(version + '\n')
    (stage / 'BUILD.txt').write_text('Version: ' + version + '\nPlatform: ' + args.platform + '\nCommit: ' + os.environ.get('GITHUB_SHA', 'local') + '\n')
    dist = ROOT / 'dist'
    dist.mkdir(exist_ok=True)
    if args.platform.startswith('linux-'):
        archive = dist / (name + '.tar.gz')
        with tarfile.open(archive, 'w:gz') as output:
            output.add(stage, arcname=name)
    elif args.platform.startswith('macos-'):
        archive = dist / (name + '.zip')
        subprocess.run(['ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(stage), str(archive)], check=True)
    else:
        archive = dist / (name + '.zip')
        # Some bundled upstream notices retain pre-1980 archive timestamps.
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, strict_timestamps=False) as output:
            for file in sorted(stage.rglob('*')):
                if file.is_file():
                    output.write(file, arcname=file.relative_to(stage.parent))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + '.sha256').write_text(f'{digest}  {archive.name}\n')
    shutil.rmtree(temporary)
    print(archive)

if __name__ == '__main__':
    main()
