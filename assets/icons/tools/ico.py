"""Write a Windows .ico from square PNGs, or list the entries of one.

Usage:
    ico.py write <out.ico> <a.png> [<b.png> ...]
    ico.py list <in.ico>

Entries are stored as PNG data, which Windows Vista and later read at every
size. Width and height bytes of 0 mean 256.
"""

import struct
import sys


def png_size(data: bytes) -> tuple[int, int]:
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError("not a PNG")
    return struct.unpack(">II", data[16:24])


def write(out: str, pngs: list[str]) -> None:
    images = []
    for path in pngs:
        data = open(path, "rb").read()
        w, h = png_size(data)
        if w != h or w > 256:
            raise ValueError(f"{path}: {w}x{h} is not a square up to 256")
        images.append((w, data))
    images.sort(key=lambda item: item[0])

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    for w, data in images:
        b = 0 if w == 256 else w
        entries += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(out, "wb") as f:
        f.write(header + entries + b"".join(data for _, data in images))


def list_entries(path: str) -> None:
    data = open(path, "rb").read()
    reserved, kind, count = struct.unpack("<HHH", data[:6])
    if reserved != 0 or kind != 1:
        raise ValueError("not an icon file")
    for i in range(count):
        w, h, _, _, planes, bpp, size, offset = struct.unpack(
            "<BBBBHHII", data[6 + 16 * i : 22 + 16 * i]
        )
        pw, ph = png_size(data[offset : offset + size])
        print(f"{w or 256}x{h or 256} {bpp}bpp png {pw}x{ph} {size} bytes")


def main() -> None:
    if sys.argv[1] == "write":
        write(sys.argv[2], sys.argv[3:])
    elif sys.argv[1] == "list":
        list_entries(sys.argv[2])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
