"""Create a signed macOS bundle from release binaries and the pinned helper."""
from pathlib import Path
import argparse
import importlib.util
import os
import plistlib
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
HELPER = 'rdx-androguard-helper'


def resolve_helper(root, explicit=None):
    helper = Path(explicit) if explicit is not None else root / 'tools/androguard-helper/target/release' / HELPER
    if not helper.is_file():
        raise SystemExit(f'Required Androguard release helper is missing: {helper}. Build with cargo build --release --locked --manifest-path {root / "tools/androguard-helper/Cargo.toml"}, or supply --helper with the matching target binary.')
    notices = root / 'third_party/androguard'
    for required in ('LICENSE', 'README.md', 'dependency-audit.json', 'vendored-source-audit.json', 'dependencies', 'mpl-source'):
        if not (notices / required).exists():
            raise SystemExit(f'Required Androguard notice/source artifact is missing: {notices / required}')
    verifier_path = root / 'tools/androguard-helper/package.py'
    spec = importlib.util.spec_from_file_location('rdx_helper_package_verifier', verifier_path)
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    try:
        verifier.verify_helper(helper, root=root)
    except (OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f'Cannot verify matching-target Androguard helper {helper}: {error}') from error
    return helper.resolve()


def package_app(binary_dir, output, helper=None, root=ROOT):
    """Preflight everything before touching the destination; return its app path."""
    binary_dir = Path(binary_dir).resolve()
    helper = resolve_helper(root, helper)
    binaries = [binary_dir / name for name in ('rdx', 'rdx-source-stats')] + [helper]
    for source in binaries:
        if not source.is_file():
            raise SystemExit(f'Release binary is missing: {source}. Run cargo build --release --bins first.')
    # Explicit CI paths prevent accidental host/target mixing. This check also
    # catches a stale host helper used with a cross-built local executable.
    architectures = [set(subprocess.check_output(['lipo', '-archs', str(source)], text=True).split()) for source in binaries]
    if any(architecture != architectures[0] for architecture in architectures[1:]):
        raise SystemExit(f'Release binary/helper architecture mismatch: {list(zip(map(str, binaries), architectures))}')
    version = tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']
    output = Path(output)
    bundle = output / 'Contents'
    executable_dir = bundle / 'MacOS'
    resources = bundle / 'Resources'
    executable_dir.mkdir(parents=True, exist_ok=True)
    resources.mkdir(parents=True, exist_ok=True)
    for source in binaries:
        name = HELPER if source == helper else source.name
        temporary = executable_dir / (name + '.new')
        shutil.copy2(source, temporary)
        os.replace(temporary, executable_dir / name)
    shutil.copy2(root / 'assets/icons/rdx.icns', resources / 'rdx.icns')
    for notice in ('LICENSE', 'NOTICE'):
        shutil.copy2(root / notice, resources / notice)
    shutil.copytree(root / 'third_party', resources / 'third_party', dirs_exist_ok=True)
    shutil.copytree(root / 'plugins', resources / 'plugins', dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns('__pycache__', '*.pyc', '.DS_Store', 'tests'))
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
    # Sign nested executables explicitly before sealing the enclosing bundle.
    for name in ('rdx', 'rdx-source-stats', HELPER):
        subprocess.run(['codesign', '--force', '--sign', '-', str(executable_dir / name)], check=True)
    subprocess.run(['codesign', '--force', '--sign', '-', str(output)], check=True)
    subprocess.run(['codesign', '--verify', '--deep', '--strict', str(output)], check=True)
    os.utime(output, None)
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary-dir', type=Path, default=ROOT / 'target/release')
    parser.add_argument('--helper', type=Path, help='Matching release helper; defaults to the local separate helper build')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/RDX.app')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        raise SystemExit('Run this packaging script on macOS.')
    app = package_app(args.binary_dir, args.output, args.helper)
    print(f'Packaged {app}')


if __name__ == '__main__':
    main()
