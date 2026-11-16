# The pages' design

zone serves three pages: the history page (`assets/index.html`), Flash (`assets/flash.html`)
and the locked page (`assets/locked.html`). They are meant to look made by the same hands as
the iPhone app and Zone for Mac (`mac/Zone/Theme.swift`). These are the decisions, so a change
can keep them.

## Color

The Mac theme's values, unchanged: paper `#2A2A2A` / `#F1E9D2`, cards `#333333` / `#F7F2E4`,
ink `#F1E9D2` / `#333333`, muted `#C9C1AB` / `#555555`, rules of ink at 16 / 18%. The pages
follow `prefers-color-scheme`; both faces are designed, neither is a filter of the other.

Purple is used sparingly, as in the apps: the wordmark's dot, the wave, the state light,
search marks, the calendar's dots, focus rings, and the tiles of the Flash picture. The main
action is an ink capsule (cream on charcoal, charcoal on cream), never purple.

Two values were added for small text, measured against the paper: "quiet" text is ink at 62%
in dark (`#A5A092`, 5.5:1) and `#686660` in light (4.7:1), and purple as small text in dark is
`#A68BFF` (5.3:1) instead of `#9B7DFF` (4.6:1). The alarm color is `#FF6B6B` / `#B3261E`, for
errors only.

## Type

A serif for the wordmark, headings and numbers; the system's sans for everything else. The
serif is Zone Serif: Charis SemiBold 7.000 (SIL Global, OFL 1.1), subset to Latin (16.8 KB),
renamed as the license's Reserved Font Name clause requires, carried in the binary and served
from `/font/zone-serif.woff2` (see `assets/fonts/README.md`). Safari gets New York first through
`ui-serif`, so in Safari and in Zone for Mac's Flash window the pages use the apps' own serif.

Why embed a font: the serif carries the identity, and system serifs differ too much. Windows
would give Cambria or Georgia (Georgia's figures are old style and uneven, wrong for counts),
Linux usually DejaVu Serif, which is wide and heavy. Charis is Bitstream Charter, which is
what Chrome on a Mac shows for the same stack anyway, so the Mac looks the same in every
browser and the ThinkPad looks like the Mac. Its figures are lining and all one width.

No monospace except for commands (`zone pair`). No all-caps labels: small labels are sentence
case, 12 to 13 px, medium weight, quiet.

## Layout

The history page is a dated document, not a stack of cards: a serif heading per day with a 1 pt
rule, the time in the margin, the words, one quiet line of facts, Copy on hover or focus. Filters
live in a rail: kinds and sources as lists with serif counts, quick ranges, and a month calendar
with a dot on days that have items (it replaces the browser's date fields; pick a day, or two for
a range; arrows and Page Up/Down move through it). The title names what is shown. In a narrow
window the title comes first, then the filters, which scroll sideways.

Flash puts the words on the left (headline, the iPhone's own three steps as a timeline, the
pairing note) and the instrument on the right: the camera and the library as flat cards joined
by the phone's sign for sending (two chevrons), the readout underneath with a large serif
percentage and the counts as serif figures set apart by 1 pt rules, as the Mac popover shows its
counts.

Surfaces are flat. Cards have a 1 pt rule and a hard shadow (2 px across, 3 px down, no blur).
No gradients on surfaces, no blur, no glow.

## Motion

One duration for state changes (0.2 s) and Zone's settle curve (`cubic-bezier(0.22, 1, 0.36, 1)`,
0.45 s) for things arriving, as in `Theme.swift`.

- The bar's wave is Zone for Mac's `FlowingWave`, ported: a still hairline until something
  arrives (new items on the history page, codes on Flash), then awake.
- New items land in purple and settle to ink (the Words prototype's idea).
- On Flash each tile lands in purple, grows into place and settles to its own tone. Complete, the
  tiles draw a voice as the iPhone draws one: bars mirrored about a middle line, purple on quieter
  ink. The last pieces resolve in order and a pass of ink runs across. Before the first code the
  picture to come is held faintly; a flash that will not open leaves the tiles quiet, ringed in
  the alarm color.
- Reduce Motion holds everything still: one frame of the wave, tiles in place, no ring or chevron
  movement.

## Rules the pages keep

Nothing is loaded from anywhere but zone itself (the policy is `default-src 'none'` with nonces;
the tests in `src/web.rs` check it). No inline `style=` attributes and no inline handlers. No em
dashes. Every control is reachable by keyboard with a visible focus ring, and the states that
change are announced (`aria-live`).
