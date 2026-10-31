"""Tests for the asset regeneration tool."""

import struct
import unittest

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


if __name__ == "__main__":
    unittest.main()
