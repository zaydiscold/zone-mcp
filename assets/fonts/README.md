# Zone Serif

The serif of zone's pages (the wordmark, headings and numbers) is `zone-serif.woff2`: Charis
SemiBold 7.000 by SIL Global, cut down to Latin letters and punctuation (268 glyphs, 16.8 KB) and
renamed Zone Serif. zone carries it inside its binary and serves it from
`/font/zone-serif.woff2`, so the pages look the same on macOS, Windows and Linux with no network
at all. Safari draws New York instead (`ui-serif`), the serif Zone for Mac and the iPhone app use.

Why Charis: it is Bitstream Charter, extended. Charter is the serif Chrome on a Mac falls back to
for the same stack, so the pages read the same in every browser there, and its figures are
lining and all one width, which the counts and times need.

## License

Charis is licensed under the SIL Open Font License 1.1, in [`OFL.txt`](OFL.txt), copyright
SIL Global, with the Reserved Font Names "Charis" and "SIL". A subset is a Modified Version under
that license, so it carries a different name (Zone Serif, in the font's own name records as well
as in the pages' CSS). The copyright and license records are kept inside the font. zone's own MIT
license does not cover this file.

## Making it again

From the release at https://github.com/silnrsi/font-charis/releases/tag/v7.000
(`Charis-7.000.zip`, SHA-256 `e3237b1303c5d31af8f59b1d1914886c5e873b77c71390e4742fb3bc1c187666`):

```sh
pip install fonttools brotli
python3 -I scripts/make-zone-serif.py Charis-7.000/Charis-SemiBold.ttf assets/fonts/zone-serif.woff2
```

The same source gives the same bytes (SHA-256
`1046178ecfc6362a9c7031f2e93e73c4623d291c935d643e00e85e3b51b92134`). The characters kept are
listed in the script; add to them there if a page needs one more.
