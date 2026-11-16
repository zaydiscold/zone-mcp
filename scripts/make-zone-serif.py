"""Zone Serif: Charis 7.000 SemiBold (SIL Global, OFL 1.1), subset to Latin and renamed.

Usage: python3 -I scripts/make-zone-serif.py Charis-SemiBold.ttf assets/fonts/zone-serif.woff2
Needs fontTools and brotli (pip install fonttools brotli). See assets/fonts/README.md.
The OFL's Reserved Font Names are "Charis" and "SIL", and a subset is a Modified Version,
so the subset carries its own name. The copyright and license records are kept.
"""
import sys
from fontTools import subset
from fontTools.ttLib import TTFont

src, out = sys.argv[1], sys.argv[2]
UNICODES = (
    "U+0020-007E,U+00A0-00FF,U+0152-0153,U+2009,U+200A,U+2011,U+2013,U+2018-201A,U+201C-201E,"
    "U+2022,U+2026,U+202F,U+2032,U+2033,U+2039,U+203A,U+20AC,U+2190-2193,U+2212"
)
options = subset.Options()
options.layout_features = ["kern", "liga", "locl", "ccmp", "mark", "mkmk", "lnum", "onum", "frac"]
options.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
options.name_languages = [0x409]
options.hinting = False
options.desubroutinize = True
options.flavor = "woff2"
# The source's timestamps are kept, so the same source always gives the same bytes.
font = TTFont(src, recalcTimestamp=False)
subsetter = subset.Subsetter(options)
subsetter.populate(unicodes=subset.parse_unicodes(UNICODES))
subsetter.subset(font)
name = font["name"]
version = name.getDebugName(5) or ""
for record in list(name.names):
    if record.nameID in (1, 2, 3, 4, 6, 16, 17):
        name.removeNames(nameID=record.nameID)
for nid, value in {
    1: "Zone Serif",
    2: "Regular",
    3: "Zone Serif SemiBold; subset of Charis 7.000",
    4: "Zone Serif SemiBold",
    6: "ZoneSerif-SemiBold",
    16: "Zone Serif",
    17: "SemiBold",
}.items():
    name.setName(value, nid, 3, 1, 0x409)
name.setName(
    "A Latin subset of Charis SemiBold 7.000 by SIL Global, renamed as the Reserved Font Name clause of the SIL Open Font License 1.1 requires.",
    10, 3, 1, 0x409,
)
# The family has one weight: SemiBold, 600.
font["OS/2"].usWeightClass = 600
font.flavor = "woff2"
font.save(out)
