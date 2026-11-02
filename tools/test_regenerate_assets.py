"""Tests for the asset regeneration tool."""

import hashlib
import io
import struct
import tempfile
import unittest
from pathlib import Path

import regenerate_assets


def fake_png(size: int) -> bytes:
    """Return bytes standing in for a PNG of the given pixel size."""
    return b"\x89PNG" + bytes([size]) * (size + 3)


class IcoBytesTest(unittest.TestCase):
    """The ICO container wraps PNG images in an ICONDIR."""

    def test_ico_header_and_directory(self) -> None:
        """The header, one entry per image and the image data line up."""
        sizes = [16, 32, 48]
        pngs = [(size, fake_png(size)) for size in sizes]

        ico = regenerate_assets.ico_bytes(pngs)

        self.assertEqual(ico[:6], bytes([0, 0, 1, 0, 3, 0]))
        offset = 6 + 16 * len(pngs)
        for index, (size, png) in enumerate(pngs):
            entry = ico[6 + 16 * index : 6 + 16 * (index + 1)]
            width, height, colours, reserved, planes, bpp, length, start = struct.unpack(
                "<BBBBHHII", entry
            )
            self.assertEqual((width, height), (size, size))
            self.assertEqual((colours, reserved, planes, bpp), (0, 0, 1, 32))
            self.assertEqual(length, len(png))
            self.assertEqual(start, offset)
            self.assertEqual(ico[start : start + length], png)
            offset += length
        self.assertEqual(len(ico), offset)

    def test_size_256_is_stored_as_zero(self) -> None:
        """A width or height of 256 is written as 0."""
        ico = regenerate_assets.ico_bytes([(256, fake_png(9))])

        self.assertEqual(ico[6:8], bytes([0, 0]))


LICENSED_UNDER = "This Font Software is licensed under the SIL Open Font License, Version 1.1."


class VerifiedTest(unittest.TestCase):
    """Downloads are checked against their pinned sha256."""

    def test_checksum_mismatch_is_refused(self) -> None:
        """A file whose digest differs from the pin is refused, naming the file."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "font.ttf"
            path.write_bytes(b"not the pinned font")
            wrong = hashlib.sha256(b"something else").hexdigest()

            with self.assertRaises(SystemExit) as raised:
                regenerate_assets.verified(path, wrong)

            self.assertIn("font.ttf", str(raised.exception))

    def test_matching_checksum_returns_the_path(self) -> None:
        """A file with the pinned digest is accepted."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "font.ttf"
            path.write_bytes(b"the pinned font")
            digest = hashlib.sha256(b"the pinned font").hexdigest()

            self.assertEqual(regenerate_assets.verified(path, digest), path)


class ReservedFontNameTest(unittest.TestCase):
    """Only the copyright header can declare a Reserved Font Name."""

    def test_plain_ofl_has_no_reserved_name(self) -> None:
        """The OFL's own definition of the term, below the header, does not count."""
        lines = (
            "Copyright 2016 The Example Project Authors (https://example.org/fonts)",
            "",
            LICENSED_UNDER,
            "This license is copied below, and is also available with a FAQ at:",
            "",
            "DEFINITIONS",
            '"Reserved Font Name" refers to any names specified as such after the',
            "copyright statement(s).",
        )
        licence = "\n".join(lines)

        self.assertFalse(regenerate_assets.declares_reserved_font_name(licence))

    def test_reserved_name_in_header_is_found(self) -> None:
        """A Reserved Font Name in the copyright header is found."""
        lines = (
            'Copyright \u00a9 2017 IBM Corp. with Reserved Font Name "Plex"',
            "",
            LICENSED_UNDER,
        )
        licence = "\n".join(lines)

        self.assertTrue(regenerate_assets.declares_reserved_font_name(licence))

    def test_licence_without_the_licensed_under_line_counts_as_reserved(self) -> None:
        """A licence whose header cannot be delimited is refused."""
        licence = "Copyright 2016 The Example Project Authors\nSome other licence text."

        self.assertTrue(regenerate_assets.declares_reserved_font_name(licence))


class ReadCappedTest(unittest.TestCase):
    """A download larger than the cap is refused, one at the cap is kept."""

    def test_a_body_at_the_cap_is_returned(self) -> None:
        """Exactly DOWNLOAD_MAX_BYTES is accepted."""
        body = b"x" * regenerate_assets.DOWNLOAD_MAX_BYTES

        self.assertEqual(regenerate_assets.read_capped(io.BytesIO(body), "https://x"), body)

    def test_a_body_over_the_cap_is_refused(self) -> None:
        """One byte more than the cap stops the tool."""
        body = b"x" * (regenerate_assets.DOWNLOAD_MAX_BYTES + 1)

        with self.assertRaises(SystemExit):
            regenerate_assets.read_capped(io.BytesIO(body), "https://x")


if __name__ == "__main__":
    unittest.main()
