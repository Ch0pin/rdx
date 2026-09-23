"""Package the approved PNG into platform icon formats (requires macOS tools)."""
from pathlib import Path
import argparse
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    output = ROOT / 'assets/icons'
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='rdx-icons-') as temp:
        iconset = Path(temp) / 'rdx.iconset'
        iconset.mkdir()
        def resize(size, destination):
            subprocess.run(['sips', '-z', str(size), str(size), str(source), '--out', str(destination)], check=True, stdout=subprocess.DEVNULL)
        for size in (16, 32, 128, 256, 512):
            resize(size, iconset / f'icon_{size}x{size}.png')
            resize(size * 2, iconset / f'icon_{size}x{size}@2x.png')
        resize(512, output / 'rdx.png')
        subprocess.run(['iconutil', '-c', 'icns', str(iconset), '-o', str(output / 'rdx.icns')], check=True)
        images = []
        for size in (16, 32, 48, 64, 128, 256):
            png = Path(temp) / f'{size}.png'
            resize(size, png)
            images.append((size, png.read_bytes()))
        # ICO supports PNG-compressed entries. A zero dimension represents 256.
        offset = 6 + 16 * len(images)
        entries = []
        for size, image in images:
            entries.append(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(image), offset))
            offset += len(image)
        (output / 'rdx.ico').write_bytes(struct.pack('<HHH', 0, 1, len(images)) + b''.join(entries) + b''.join(image for _, image in images))
    print('Created assets/icons/rdx.png, rdx.icns and rdx.ico')

if __name__ == '__main__':
    main()
