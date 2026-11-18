# Logic pass, 2026-11-18

A skeptical pass over 0.1.3: the page (`assets/index.html`, `src/web.rs`), the paths an update
takes, and Zone for Mac's popover. Everything was run against throwaway libraries
(`ZONE_HOME` in a temporary folder, ports 47901 to 47941), a synthetic library from
`zone dev synthetic --demo` sent by `zone dev fake-phone`, a second phone sending hostile
items, and the page driven in headless Chrome. The update paths are written up in
[docs/updating.md](../updating.md).

Severity: **high** loses or exposes words, or leaves people on an old version without
knowing; **medium** shows something wrong or contradicts itself; **low** is cosmetic or needs
an unlikely setup.

Status is "fixed in" a commit, or "left" with the reason.

## Updates

| # | What | How to see it | Severity | Status |
|---|---|---|---|---|
| U1 | After an update the zone that is running is still the old one unless a login item restarts it, and nothing names the running version. | 0.1.1 `zone serve` on a spare port, `zone update` beside it: file is 0.1.3, process is the same pid, `zone status` says only "running". | high | fixed in 750a594 (`zone status` and `zone doctor` name the running version) and 6ce7468 (a running zone starts the version that replaced it, on macOS and Linux; test `a_running_zone_starts_the_version_that_replaced_it`). Reaches people from the release after 0.1.3: a 0.1.1 to 0.1.3 zone still has to be stopped and started once. |
| U2 | A page left open never notices that zone was replaced. It keeps running the old page's script against the new zone. | Open the page, restart zone as another version, wait: nothing happens until a manual reload. | high | fixed in b50b2b7: the page carries the version that served it and loads the new page when a status answer names another (seen: a 0.1.3 page on Notes became the 0.1.4 page on Notes four seconds after the program was replaced). |
| U3 | Nobody is told a newer release exists unless they run `zone update` or `zone doctor` or open Settings. | Read `src/web.rs` (`/api/update` is asked by Settings only) and `src/update.rs`. | high | fixed in 73aa116 for the page: it asks when opened and every six hours (zone still keeps GitHub's answer twelve hours) and says once per version "zone X is available", until answered. This widens what README promised from "opening Settings" to "while the page is open": README says so now, and the owner should confirm it is wanted. Left: the command line says nothing unless `zone update` or `zone doctor` is run, and Zone for Mac only asks when its Settings come into view. |
| U4 | `zone update` gives up at GitHub's API limit although the download needs no API. | `ZONE_UPDATE_URL=file://.../rate-limit.json zone update --check`: "GitHub answered: API rate limit exceeded", exit 1. | medium | fixed in 7a2df1c: when the API answers with a refusal, the website's `/releases/latest` is asked, as the install script does. |
| U5 | Zone for Mac's Update button opens a release page that has no `Zone.dmg` on it. | v0.1.3's assets: four archives, `SHA256SUMS`, `zone.rb`. | high | fixed in ac2a9f5: Update is offered only when the release has a `.dmg` on it and then downloads that file; otherwise the line says the app is not out yet. Seen in the app's sandbox with both kinds of release. Reaches people with the next app build; for the 0.1.2 app in people's hands the owner attaches `Zone.dmg` to v0.1.3. |
| U6 | Opening Zone for Mac replaces a newer zone (from `zone update`) with the older one inside the app. | `BinaryInstaller.ensure` replaces whenever the two files differ. | medium | fixed in 102dcf0: a copy that is a later version than the app's is left alone (test `testANewerCopyIsNeverReplacedByTheOlderOneInsideTheApp`; seen in the sandbox: copy replaced by 0.1.4, app reopened, copy still 0.1.4). |
| U7 | Zone for Mac shows its own copy's version, not the version of the zone that is running, when a login item runs another copy. | Seen live: 0.1.3 running from `~/.local/bin`, the app's copy 0.1.2. `zone status --json` had no version for the running zone. | medium | fixed in 8206944: "Zone 0.1.3. Running: zone 0.1.4" at the foot of Settings, from `daemon.version` (750a594). |
| U8 | After going back to an older zone and forward again, an item can be both in the library and in "Removed from this computer", where Put back does nothing. | 0.1.3: remove a note. 0.1.1 zone on the same library, phone resends. 0.1.3 again: total 60, removed 1. | low | fixed in 0a4191b: when the library is opened, an item that is both in the library and among the removed is removed again if it is the version that was removed (or older), and is back for good if the phone edited it since. |
| U9 | `install.sh` deletes the old `zone` and then moves the new one in: for a moment there is no `zone` at the path, and an agent starting `zone mcp` just then fails. | Read `install.sh` (`rm -f "$target"` then `mv`). | low | fixed in abc93b5: one `mv -f`, no `rm` first. |
| U10 | `src/update.rs` says "The daemon never asks", and it does, for the page's Settings. | Read. | low | fixed in 73aa116. |

Checked and sound: the checksum is verified before anything is replaced; the download is run
(`--version`) before it replaces anything; a new file is written beside the old one and
moved, never written into a running program; drafts and prereleases are never "latest";
0.1.1 and 0.1.2 programs read and write a library that 0.1.3 runs; both upgrade in place.

## The page

| # | What | How to see it | Severity | Status |
|---|---|---|---|---|
| P1 | Removing a note brings the recording behind it into view: the same words reappear as a dictation in History, in search, in exports and in the counts. | Remove one demo note: `folded` 50 to 49, dictations 327 to 328, "Note recording" 1 to 2, a search for the note's words still finds them. | high | fixed in 108221a: removing a note takes the recording behind it along and putting it back returns both (test `removing_a_note_takes_the_recording_behind_it_along`). |
| P2 | CSV exports let a spreadsheet run a formula: text beginning with `=`, `+`, `-` or `@` is written as it is. | Export an item whose text is `=HYPERLINK("http://evil","x")` as CSV and open it in a spreadsheet. | medium | fixed in bb4561b: in CSV, an id, kind, source, title or text that begins with `=`, `+`, `-`, `@`, a tab or a return gets an apostrophe in front. The command line's CSV is the same writer. |
| P3 | On Notes, the heading ("56 open, 0 done") does not change when a note is ticked, while the tab's count does. | Tick one note: tab says 55, heading still says 56 open, 0 done. | medium | fixed in d829083. |
| P4 | An open meeting does not change when the phone sends the finished transcript. | Open a meeting that is still transcribing, send it again with a title and text: six seconds later the page still shows the empty one. | medium | fixed in ed9ec43: the open item is asked for again when the library changes and drawn again only when it differs. Seen: an empty meeting became "Roadmap sync" with its turns within seven seconds. The same commit fixes a second fault found on the way: a change that arrived while something was selected, a dialog was open or a phone was being renamed was never drawn afterwards. |
| P5 | History's counts beside the kinds do not follow "Show them": with the recordings shown the heading says 471 items and "Everything" still says 421. | `#/history?fold=0`. | medium | fixed in 8120a61. |
| P6 | A link or a remembered filter that names a phone which is no longer paired breaks History: "Could not load this. no such paired phone. Is zone still running?" | `#/history?phone=<an unpaired id>`. | medium | fixed in edf582f. |
| P7 | The command line and agents list and export the recordings behind notes; the page leaves them out. One library, two counts. | `zone export --format json` has 51 `source: note` items where the page's export has 1. | low | open |
| P8 | An empty library's statistics say "Time spoken 1 s". | Statistics on an empty library. | low | fixed in 44ba642. |
| P9 | Pressing `e` on Settings opens an export of whatever History last showed. | Settings, press `e`. | low | fixed in 44ba642. |
| P10 | Markdown exports carry the words as they are, so a note that contains HTML is HTML to a Markdown viewer that allows it. | Export a note containing `<img src=x onerror=...>` as Markdown. | low | left: escaping would change the person's words in a file meant to be read as text, and Markdown viewers that render raw HTML from a local file are the reader's choice. The page itself never renders words as HTML (checked below). |
| P11 | An item whose id contains a comma cannot be exported or opened by id (ids are sent comma separated). | A fake phone sending id `A,B`. | low | left: the phone's ids are UUIDs. |

Checked and sound, by experiment:

- **Security of the local page.** Without the key every route answers 401 except the font
  file. Every write is a POST that needs the key and is refused with any other `Origin`
  (another local port and `null` included) or `Sec-Fetch-Site: cross-site`; a GET cannot
  write (405); another `Host` is refused. The key leaves the address bar at once (redirect to
  `/` with a `HttpOnly; SameSite=Strict` cookie, `referrer-policy: no-referrer`,
  `cache-control: no-store`) and is not in the log. The data folder's files are `0600`.
- **Words are never HTML.** Items holding `<script>`, `<img onerror>`, `javascript:` links, a
  hostile title, hostile speaker names, hostile kinds and sources, and a hostile id were shown
  on every view and route: no script ran. The policy on the page allows only its own script
  by nonce.
- **Done** agrees between the page, `mark_processed` over MCP and the command line, survives
  remove and put back, and survives an edit on the phone.
- **Remove and put back**: a removed item stays away when Link resends it in the same or an
  older version, comes back when the phone edits it (as the page says), is simply deleted
  when the phone deletes it, and counts never went negative.
- **Phones**: a nickname survives the phone renaming itself; two phones with one name are
  told apart by id on the page; unpairing keeps the items and re-pairing the same phone gets
  them back under its filter.
- **Keys**: letters typed into the search field or a rename field are text, not shortcuts.
  Back and forward walk the routes.
- **Hostile routes**: unknown routes fall back to home, unknown ids say the item is not here.
- **Speed at 100,000 items**: home loads in 2.3 s in headless Chrome, statistics in 0.34 s,
  History in 0.16 s, a search in 0.16 s; `/api/status` 0.33 s, `/api/stats` 0.17 s.

## Zone for Mac

| # | What | How to see it | Severity | Status |
|---|---|---|---|---|
| M1 | The last meeting sits at the top of the popover as a bare title. Only the tooltip and VoiceOver say "Last meeting". | `mac/Zone/PopoverView.swift`, `pinned`. | medium | fixed in 4997016: the words "Last meeting" stand before the title, and a meeting without a title is "Untitled" there (seen in the app's sandbox). |

## Not verified

Listed at the end of the pass.
