"""Regenerate the binary assets under crates/server/assets from their sources.

Usage: python3 -I tools/regenerate_assets.py icons|fonts

`icons` renders the SVG sources to PNG and ICO with resvg (version pinned
below).

`fonts` downloads the pinned Google Fonts sources, checks their sha256, refuses
any font whose copyright header declares a Reserved Font Name, then instances
and subsets each to woff2 with fontTools (hash-pinned in
tools/requirements-fonts.txt) and copies its licence next to it.

Generated files are never edited by hand.
"""

import hashlib
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path

RESVG_VERSION = "0.48.1"
ROOT = Path(__file__).resolve().parent.parent
TOOLS = ROOT / "tools"
ICONS_OUT = ROOT / "crates" / "server" / "assets" / "icons"
FONTS_OUT = ROOT / "crates" / "server" / "assets" / "fonts"
FONT_REQUIREMENTS = TOOLS / "requirements-fonts.txt"

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


LICENSED_UNDER = "This Font Software is licensed under the SIL Open Font License, Version 1.1."
RESERVED_FONT_NAME = "Reserved Font Name"
GOOGLE_FONTS = "https://raw.githubusercontent.com/google/fonts"
DOWNLOAD_TIMEOUT = 60
# The unicode-range of every @font-face in the theme files.
UNICODE_RANGES = (
    "U+0000-024F",
    "U+0259",
    "U+02BB-02BC",
    "U+02C6",
    "U+02DA",
    "U+02DC",
    "U+0300-0304",
    "U+0308",
    "U+0329",
    "U+1E00-1EFF",
    "U+2000-206F",
    "U+20A0-20C0",
    "U+2113",
    "U+2122",
    "U+2190-2193",
    "U+2212",
    "U+2215",
    "U+FEFF",
    "U+FFFD",
)
LAYOUT_FEATURES = "kern,liga,calt,tnum,lnum,case,ccmp,locl,mark,mkmk"


@dataclass(frozen=True)
class FontSource:
    """A variable font in the google/fonts repository, pinned by commit and sha256."""

    family: str
    commit: str
    directory: str
    file: str
    sha256: str
    licence_sha256: str
    axes: tuple[str, ...]
    output: str

    def url(self, file: str) -> str:
        """Return the raw URL of a file in this font's directory at the pinned commit."""
        return f"{GOOGLE_FONTS}/{self.commit}/ofl/{self.directory}/{file}"


FONT_SOURCES = (
    FontSource(
        family="fredoka",
        commit="a60a77e14f28abd4ef243a1b5dfc48df0cec5205",
        directory="fredoka",
        file="Fredoka[wdth,wght].ttf",
        sha256="2ba02e68b152868aef9ba28e24b3648c7d457fe6f25c761f2c2c53fb61a73fc8",
        licence_sha256="5c9e7eee5c6b25f4b05b8d53b2e470ea4962f9ced742d044a98f7d95d1375bab",
        axes=("wdth=100", "wght=600"),
        output="fredoka-600.woff2",
    ),
    FontSource(
        family="nunito",
        commit="8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5",
        directory="nunito",
        file="Nunito[wght].ttf",
        sha256="bb55a5ca5c2042335b3991af27c4d0705d0ef41cac6164ac737fd8f2a1e85207",
        licence_sha256="580df76c95a1ec5ab878ceb25bb3d85c6a076804e9c970c8c6972aea775fdf65",
        axes=("wght=400:800",),
        output="nunito-400-800.woff2",
    ),
    FontSource(
        family="manrope",
        commit="b31870aff700ab7a1d74fa0c6887d95beb9e0037",
        directory="manrope",
        file="Manrope[wght].ttf",
        sha256="3ae11c49db0455a3cc33e37d380f20fdb8c7f8b41dc07625c177e3d87a9d6ae6",
        licence_sha256="58172e0c0fac2cda8a37b348164bb55e44b0e69051e557e92b1d3f6910141f7b",
        axes=("wght=400:800",),
        output="manrope-400-800.woff2",
    ),
)


def verified(path: Path, sha256: str) -> Path:
    """Return `path` if its sha256 is `sha256`; refuse it otherwise."""
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != sha256:
        message = f"{path.name}: sha256 {digest} does not match the pinned {sha256}"
        raise SystemExit(message)
    return path


def declares_reserved_font_name(licence: str) -> bool:
    """Whether the licence's copyright header declares a Reserved Font Name.

    The header is every line before the OFL's "licensed under" line; the OFL's
    own definition of the term, further down, does not count. A licence without
    that line cannot be checked, so it counts as declaring one.
    """
    header = []
    for line in licence.splitlines():
        if line.strip() == LICENSED_UNDER:
            return any(RESERVED_FONT_NAME in text for text in header)
        header.append(line)
    return True


def download(url: str, destination: Path) -> Path:
    """Download an https URL to `destination`."""
    if not url.startswith("https://"):
        message = f"refusing a non-https URL: {url}"
        raise SystemExit(message)
    # The scheme is checked above and every URL is built from GOOGLE_FONTS.
    with urllib.request.urlopen(url, timeout=DOWNLOAD_TIMEOUT) as reply:  # noqa: S310
        destination.write_bytes(reply.read())
    return destination


def font_tools(work: Path) -> Path:
    """Install the hash-pinned fontTools into a fresh virtual environment; return its bin."""
    venv = work / "venv"
    subprocess.run(["uv", "venv", "--quiet", str(venv)], check=True)
    python = venv / "bin" / "python"
    install = ["uv", "pip", "install", "--quiet", "--python", str(python), "--require-hashes"]
    subprocess.run([*install, "-r", str(FONT_REQUIREMENTS)], check=True)
    return venv / "bin"


def fonts() -> None:
    """Write the subset woff2 fonts and their licences."""
    with tempfile.TemporaryDirectory() as directory:
        work = Path(directory)
        licences = {}
        sources = {}
        for font in FONT_SOURCES:
            licence = download(font.url("OFL.txt"), work / f"ofl-{font.family}.txt")
            verified(licence, font.licence_sha256)
            if declares_reserved_font_name(licence.read_text(encoding="utf-8")):
                message = f"{font.family}: the copyright header declares a Reserved Font Name"
                raise SystemExit(message)
            licences[font.family] = licence
            source = download(font.url(font.file), work / f"{font.family}-source.ttf")
            sources[font.family] = verified(source, font.sha256)
        bin_dir = font_tools(work)
        environment = {**os.environ, "SOURCE_DATE_EPOCH": "0"}
        FONTS_OUT.mkdir(parents=True, exist_ok=True)
        for font in FONT_SOURCES:
            instance = work / f"{font.family}-instance.ttf"
            subprocess.run(
                [
                    str(bin_dir / "fonttools"),
                    "varLib.instancer",
                    str(sources[font.family]),
                    *font.axes,
                    "-o",
                    str(instance),
                ],
                check=True,
                env=environment,
            )
            subprocess.run(
                [
                    str(bin_dir / "pyftsubset"),
                    str(instance),
                    f"--unicodes={','.join(UNICODE_RANGES)}",
                    f"--layout-features={LAYOUT_FEATURES}",
                    "--flavor=woff2",
                    "--name-IDs=*",
                    "--name-languages=*",
                    f"--output-file={FONTS_OUT / font.output}",
                ],
                check=True,
                env=environment,
            )
            shutil.copyfile(licences[font.family], FONTS_OUT / f"ofl-{font.family}.txt")
    print("fonts written")


def main(argv: list[str]) -> int:
    """Run the subcommand named in argv."""
    if argv == ["icons"]:
        icons()
        return 0
    if argv == ["fonts"]:
        fonts()
        return 0
    print("usage: regenerate_assets.py icons|fonts", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
