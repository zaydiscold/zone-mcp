# The pages' design

zone serves three pages: the library page (`assets/index.html`), Flash (`assets/flash.html`)
and the locked page (`assets/locked.html`). They are meant to look made by the same hands as
the iPhone app and Zone for Mac (`mac/Zone/Theme.swift`). These are the decisions, so a change
can keep them.

## Color

The Mac theme's values, unchanged: paper `#2A2A2A` / `#F1E9D2`, cards `#333333` / `#F7F2E4`,
ink `#F1E9D2` / `#333333`, muted `#C9C1AB` / `#555555`, rules of ink at 16 / 18%. Both faces
are designed, neither is a filter of the other. Flash and the locked page follow
`prefers-color-scheme`. The library page follows what Settings says: this computer's setting
(`data-theme="auto"`), Light, Dark, or the iPhone's, once a phone has said how Zone looks on
it (PROTOCOL.md, `hello`). The server writes `data-theme` and `data-accent` into the page before
it is sent, so it never flashes the wrong face.

The accent is used sparingly, as in the apps: the wordmark's dot, the state light, search
marks, the calendar's dots, focus rings, your own turns in a meeting, and the tiles of the Flash
picture. The main action is an ink key (cream on charcoal, charcoal on cream), never the accent. On the library page
the accent is one of the iPhone app's five (`AccentChoice`: violet, ice, ember, mint, rose),
with the app's own three tones for each: the bright one on dark paper, the deeper graphic tone
as shapes on cream, the text tone as text on cream. Until one is chosen in Settings it is the
iPhone's, when the phone says, else violet.

Kinds have no colors of their own (2026-11-18, "Paper"): the iPhone app has one accent and
none per kind, and the amber, teal and blue stripes read as decoration. A kind shows in its quiet
icon and in words (the kind's label in the row's facts, a note's round checkbox).

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

The library page is a tool to work from, not a document to read. An earlier version was one
dated document for everything (a serif heading per day, the time in the margin, one quiet line
of facts): every item looked the same whatever it was, notes and meetings were a filter away,
and nothing could be acted on. What it is now:

- A bar with the views as tabs (Home, Notes with its open count, Meetings, History, Stats,
  Settings), search, the phone chip, Flash, and the keys.
- Home fills one laptop screen without scrolling the page: the figures straight on the paper
  (today, the last 7 days, time spoken, the busiest hour, a bar per day for 30 days) over a rule,
  then three columns that scroll by themselves. A column is a sans semibold header over a rule,
  then its list on the paper: no panel, no box. Notes, the widest, is the list of things to do. Meetings is beside it.
  "Lately" (the latest clip and recent dictations) is the narrowest and plainly secondary. Below
  1180 px Lately moves under the other two, below 720 px everything is one column.
- One row for an item everywhere, on the paper with a hairline from the next (never its own
  card): the kind's quiet icon (a round checkbox for a note), the words, one quiet line of facts
  (kind, where it was spoken, when, how long, which phone when there are several), and its actions
  on hover, focus or the keyboard's cursor: done, copy, export, remove, select. Remove takes the
  item off this computer only and offers Undo. A meeting's row has its title in sans semibold and
  its speakers as chips. Meetings in their own view are the same rows in two columns.
- Notes show their whole text, cut only after ten lines with a way to show the rest. Done notes
  are struck through and stay where they were until the view is opened again, so a tick can be
  undone where it was made.
- History keeps the dated document (a sans semibold heading per day with a rule) and the rail: kinds
  with their icons and counts, where a dictation was spoken (with what each name means), done or
  not, quick ranges, and the month calendar with a dot on days that have items. The title names
  what is shown.
- A meeting opens as a page: title, facts, actions, then the turns, the speaker and the time in
  the margin. Speakers are told apart by name; only you ("Me") are in the accent, as in the app.
- Statistics and Settings keep cards (a group of settings is a thing set apart), with the app's
  16 pt continuous corner and serif figures.
- Source names are plain words: Keyboard (spoken into another app with the Zone keyboard), In
  Zone (dictated inside the Zone app), Note recording (the recording behind a voice note). The
  meaning is in each label's tooltip and listed under "What these mean" in History.

Flash puts the words on the left (headline, the iPhone's own three steps as a timeline, the
pairing note) and the instrument on the right: the camera and the library as flat cards joined
by the phone's sign for sending (two chevrons), the readout underneath with a large serif
percentage and the counts as serif figures set apart by 1 pt rules, as the Mac popover shows its
counts.

Surfaces are flat. Three shapes, the iPhone app's: a key (8 pt corner) for every button, with a
1 pt rule inside and the hard shadow at the key's scale, sitting down into it when pressed; a card
(16 pt) only for a group of settings or a figure set apart; a capsule for the switch (an ink
capsule in a card track, 3 px in). Corners are continuous where the browser draws `corner-shape:
squircle` (the radius then grows by 1.25), circular elsewhere. The shadow is the app's sun mid
morning: 1.6 px across, 2.2 px down, no blur. Serif is for the wordmark, a view's headline,
figures and sheet titles only; list titles and section headers are the sans, semibold. No
gradients on surfaces, no blur, no glow. The same rules hold in Zone for Mac's menu window
(`mac/Zone/Theme.swift`: `ZoneKeyButtonStyle`, `zoneCard`, `Zone.quiet`).

## Motion

One duration for state changes (0.2 s) and Zone's settle curve (`cubic-bezier(0.22, 1, 0.36, 1)`,
0.45 s) for things arriving, as in `Theme.swift`.

- Flash's bar has Zone for Mac's `FlowingWave`, ported: a still hairline until codes arrive,
  then awake. The library page had it too and no longer does: a line that meant nothing most of
  the time was read as a fault. There the state light beside the phone's name does the job. It
  is filled when a phone is paired, hollow when none is, and pulses twice when something arrives
  from a phone. Its tooltip says so.
- New items land in the accent and settle to ink (the Words prototype's idea).
- On Flash each tile lands in purple, grows into place and settles to its own tone. Complete, the
  tiles draw a voice as the iPhone draws one: bars mirrored about a middle line, purple on quieter
  ink. The last pieces resolve in order and a pass of ink runs across. Before the first code the
  picture to come is held faintly; a flash that will not open leaves the tiles quiet, ringed in
  the alarm color.
- Reduce Motion holds everything still: one frame of the wave, tiles in place, no ring, pulse
  or chevron movement.

## Rules the pages keep

Nothing is loaded from anywhere but zone itself (the policy is `default-src 'none'` with nonces;
the tests in `src/web.rs` check it). No inline `style=` attributes and no inline handlers. No em
dashes. Every control is reachable by keyboard with a visible focus ring, and the states that
change are announced (`aria-live`).

The library page adds three of its own:

- Agents can never edit or delete the person's words, and neither can the page delete anything
  on the phone. Remove hides an item on this computer and says so where it is offered ("Removed
  from this computer. Still on your iPhone."), with Undo and thirty days to put it back.
- Every view and filter is in the address (docs/page-routes.md), so anything on screen can be
  linked to and the browser's Back works.
- Nothing decorative that does not inform. If a thing moves or is colored, it means something
  a tooltip can say in a sentence.
