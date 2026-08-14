"""Rebuild BasiliskOS PNG/ICO tiles with an opaque navy plate.

Windows uses the 16px and 32px frames for the taskbar and tray. Transparent
cream-on-clear tiles disappear on a dark shell. This script paints every size
on #061018 so the mark stays visible.
"""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "icons"
SOURCE = ROOT / "128x128.png"
# Lighter than the Windows 11 dark taskbar (#1c1c1c) so 16/32 tiles stay visible.
PLATE = (26, 48, 58, 255)
RING = (199, 90, 49, 255)
SIZES = (16, 24, 32, 48, 64, 128, 256)


def plate(size: int, mark: Image.Image) -> Image.Image:
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(out)
    radius = max(2, size // 5)
    box = (0, 0, size - 1, size - 1)
    draw.rounded_rectangle(box, radius=radius, fill=PLATE, outline=RING, width=max(1, size // 16))
    pad = max(2, round(size * 0.16))
    inner = size - (pad * 2)
    scaled = mark.resize((inner, inner), Image.Resampling.LANCZOS)
    out.paste(scaled, (pad, pad), scaled)
    return out


def main() -> None:
    mark = Image.open(SOURCE).convert("RGBA")
    frames = {size: plate(size, mark) for size in SIZES}
    frames[32].save(ROOT / "32x32.png")
    frames[64].save(ROOT / "64x64.png")
    frames[128].save(ROOT / "128x128.png")
    frames[256].save(ROOT / "128x128@2x.png")
    frames[256].save(ROOT / "icon.png")
    ico_order = (16, 24, 32, 48, 64, 256)
    write_ico(ROOT / "icon.ico", {size: frames[size] for size in ico_order})
    print("wrote", ROOT / "icon.ico")


def write_ico(path: Path, frames: dict[int, Image.Image]) -> None:
    sizes = sorted(frames)
    blobs: list[bytes] = []
    entries: list[tuple[int, int, int]] = []
    offset = 6 + 16 * len(sizes)
    for size in sizes:
        buf = io.BytesIO()
        frames[size].save(buf, format="PNG")
        data = buf.getvalue()
        entries.append((size, len(data), offset))
        blobs.append(data)
        offset += len(data)
    out = bytearray(struct.pack("<HHH", 0, 1, len(sizes)))
    for size, length, start in entries:
        side = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, length, start)
    for blob in blobs:
        out += blob
    path.write_bytes(out)


if __name__ == "__main__":
    main()
