"""Generate Nanobug Grid from Maple Mono NL CN Regular v7.9 using fonttools 4.60.1.

Get the source font from the MapleMonoNL-CN.zip archive at
https://github.com/subframe7536/maple-font/releases/tag/v7.9.
"""

import argparse
from hashlib import file_digest
from pathlib import Path
from unicodedata import name

from fontTools.misc.roundTools import otRound
from fontTools.ttLib import TTFont


# The pinned face has simple, unhinted Han outlines with no dependent composites.
SOURCE_SHA256 = "bb49cab8933a24b3727ca41434bf2dff923dd45e87ffcccc7c35a83c3580c2ec"
# Maximum increase in ink width and minimum side bearing as a fraction of one em.
MAX_INK_GROWTH = 0.08
SIDE_BEARING_EM = 0.04


def is_han(codepoint: int) -> bool:
    """Return whether the Unicode codepoint is a Han ideograph or ideographic zero."""
    return codepoint == 0x3007 or name(chr(codepoint), "").startswith(
        ("CJK UNIFIED IDEOGRAPH-", "CJK COMPATIBILITY IDEOGRAPH-")
    )


def widen_han(font: TTFont) -> None:
    """Widen Han-only ink in font around each glyph's center, preserving its advance."""
    cmap = font.getBestCmap()
    han = {glyph for codepoint, glyph in cmap.items() if is_han(codepoint)}
    shared = {glyph for codepoint, glyph in cmap.items() if not is_han(codepoint)}
    glyf = font["glyf"]
    metrics = font["hmtx"]
    side_bearing = otRound(font["head"].unitsPerEm * SIDE_BEARING_EM)

    for glyph_name in sorted(han - shared):
        glyph = glyf[glyph_name]
        x_min = glyph.xMin
        width = glyph.xMax - x_min
        advance, left = metrics[glyph_name]
        right = advance - left - width
        growth = min(width * MAX_INK_GROWTH / 2, left - side_bearing, right - side_bearing)
        if growth <= 0:
            continue

        center = x_min + width / 2
        scale = 1 + 2 * growth / width
        for index, (x, y) in enumerate(glyph.coordinates):
            glyph.coordinates[index] = (otRound(center + (x - center) * scale), y)
        glyph.recalcBounds(glyf)
        metrics[glyph_name] = (advance, left + glyph.xMin - x_min)


def build(source: Path, output: Path) -> None:
    """Write the derived font to output using the verified upstream source file."""
    with source.open("rb") as stream:
        if file_digest(stream, "sha256").hexdigest() != SOURCE_SHA256:
            raise ValueError("Expected MapleMonoNL-CN-Regular.ttf from the official v7.9 release")

    with TTFont(source, recalcTimestamp=False) as font:
        widen_han(font)
        # Give the derivative a distinct family and font identity while retaining the license metadata.
        names = {
            1: "Nanobug Grid",
            3: "NanobugGrid-Regular-7.900",
            4: "Nanobug Grid Regular",
            6: "NanobugGrid-Regular",
            16: "Nanobug Grid",
            21: "Nanobug Grid",
        }
        for record in font["name"].names:
            if record.nameID in names:
                record.string = names[record.nameID]
        font.save(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Upstream MapleMonoNL-CN-Regular.ttf")
    parser.add_argument(
        "output",
        type=Path,
        nargs="?",
        default=Path(__file__).resolve().parents[1] / "assets/fonts/NanobugGrid-Regular.ttf",
    )
    args = parser.parse_args()
    build(args.source, args.output)
