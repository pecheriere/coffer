"""Regenerate the binary assets under crates/server/assets from their sources.

Usage: python3 -I tools/regenerate_assets.py icons

`icons` renders the SVG sources to PNG and ICO with resvg (version pinned
below). Generated files are never edited by hand.
"""

import struct
import subprocess
import sys
from pathlib import Path

RESVG_VERSION = "0.48.1"
ROOT = Path(__file__).resolve().parent.parent
TOOLS = ROOT / "tools"
ICONS_OUT = ROOT / "crates" / "server" / "assets" / "icons"

ICONDIR_HEADER = struct.Struct("<HHH")
ICONDIR_ENTRY = struct.Struct("<BBBBHHII")
ICO_TYPE_ICON = 1
ICO_MAX_DIMENSION = 256


def ico_bytes(pngs: list[tuple[int, bytes]]) -> bytes:
    """Wrap square PNG images, given as (pixel size, bytes), in an ICO container."""
    header = ICONDIR_HEADER.pack(0, ICO_TYPE_ICON, len(pngs))
    offset = len(header) + ICONDIR_ENTRY.size * len(pngs)
    entries = []
    for size, png in pngs:
        dimension = 0 if size >= ICO_MAX_DIMENSION else size
        entries.append(ICONDIR_ENTRY.pack(dimension, dimension, 0, 0, 1, 32, len(png), offset))
        offset += len(png)
    return header + b"".join(entries) + b"".join(png for _, png in pngs)


def check_resvg() -> None:
    """Fail unless the installed resvg is the pinned version."""
    result = subprocess.run(["resvg", "--version"], capture_output=True, text=True, check=True)
    found = result.stdout.strip()
    if found != RESVG_VERSION:
        message = f"resvg {RESVG_VERSION} is required, found {found!r}"
        raise SystemExit(message)


def render(svg: Path, size: int) -> bytes:
    """Render an SVG to a square PNG of the given size and return its bytes."""
    result = subprocess.run(
        ["resvg", "--width", str(size), "--height", str(size), str(svg), "-c"],
        capture_output=True,
        check=True,
    )
    return result.stdout


def icons() -> None:
    """Write the favicon, touch icons and maskable icon."""
    check_resvg()
    favicon = ICONS_OUT / "favicon.svg"
    ico = ico_bytes([(size, render(favicon, size)) for size in (16, 32, 48)])
    (ICONS_OUT / "favicon.ico").write_bytes(ico)
    app_icon = TOOLS / "icons" / "app-icon.svg"
    for name, size in (("apple-touch-icon", 180), ("icon-192", 192), ("icon-512", 512)):
        (ICONS_OUT / f"{name}.png").write_bytes(render(app_icon, size))
    (ICONS_OUT / "icon-maskable-512.png").write_bytes(render(TOOLS / "icons" / "maskable.svg", 512))
    print("icons written")


def main(argv: list[str]) -> int:
    """Run the subcommand named in argv."""
    if argv == ["icons"]:
        icons()
        return 0
    print("usage: regenerate_assets.py icons", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
