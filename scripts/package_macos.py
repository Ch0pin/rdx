"""Create/update target/RDX.app from an existing macOS release build."""
from pathlib import Path
import argparse
import os
import plistlib
import shutil
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary-dir', type=Path, default=ROOT / 'target/release')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        raise SystemExit('Run this packaging script on macOS.')
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    binary = args.binary_dir / 'rdx'
    if not binary.is_file():
        raise SystemExit('Run cargo build --release --bins first.')
    bundle = ROOT / 'target/RDX.app/Contents'
    executable_dir = bundle / 'MacOS'
    resources = bundle / 'Resources'
    executable_dir.mkdir(parents=True, exist_ok=True)
    resources.mkdir(parents=True, exist_ok=True)
    for name in ('rdx', 'rdx-source-stats'):
        source = binary.with_name(name)
        if source.is_file():
            temporary = executable_dir / (name + '.new')
            shutil.copy2(source, temporary)
            os.replace(temporary, executable_dir / name)
    shutil.copy2(ROOT / 'assets/icons/rdx.icns', resources / 'rdx.icns')
    shutil.copytree(ROOT / 'third_party', resources / 'third_party', dirs_exist_ok=True)
    info = {
        'CFBundleExecutable': 'rdx',
        'CFBundleIdentifier': 'dev.rdx.desktop',
        'CFBundleName': 'RDX',
        'CFBundleDisplayName': 'RDX',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': version,
        'CFBundleVersion': version,
        'CFBundleIconFile': 'rdx.icns',
        'NSHighResolutionCapable': True,
        'LSMinimumSystemVersion': os.environ.get('MACOSX_DEPLOYMENT_TARGET', '13.0'),
    }
    with (bundle / 'Info.plist').open('wb') as file:
        plistlib.dump(info, file)
    os.utime(bundle.parent, None)
    print(f'Packaged {bundle.parent} ({version})')

if __name__ == '__main__':
    main()
