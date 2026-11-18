# Logic pass, 2026-11-18

A skeptical pass over 0.1.3: the page (`assets/index.html`, `src/web.rs`), the paths an update
takes, and Zone for Mac's popover. Everything was run against throwaway libraries
(`ZONE_HOME` in a temporary folder, ports 47901 to 47941), a synthetic library from
`zone dev synthetic --demo` sent by `zone dev fake-phone`, a second phone sending hostile
items, and the page driven in headless Chrome. Zone for Mac was built into a folder of its own
and run with `mac/scripts/dev-sandbox.sh`. The update paths are written up in
[docs/updating.md](../updating.md). The pass was done in several sittings: the rig that
remains is in `zone-review-2026-11-18/rig/` beside the checkout (released programs with their
checksums, the scripts, the screenshots).

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
| U11 | `zone status` (which Zone for Mac asks every few seconds) holds the daemon's lock for an instant while it looks. A zone starting in that instant concluded that another zone was running and gave up. | Found as a test that failed one run in many (`lock_shows_a_running_daemon`); then a loop of looks beside a loop of starts. | low | fixed in 922877a: a refusal is asked about again before it counts (test `a_look_at_the_lock_does_not_turn_a_starting_daemon_away`). |
| U12 | A stop signal in the moment between zone saying it is running and zone listening for signals ended it without cleaning up, and with the wrong exit status for the login item. | `a_signal_stop_is_not_a_successful_exit` failed one full run in several on a busy machine. | low | fixed in 6f30417: zone listens before it says it is running. |
| U13 | `install.ps1` moves the running `zone.exe` aside and then copies the new one in: an instant with no `zone.exe` at the path, as U9. | Read `install.ps1`. | low | left: not changed without a Windows machine to try it on. |

Checked and sound: the checksum is verified before anything is replaced; the download is run
(`--version`) before it replaces anything; a new file is written beside the old one and
moved, never written into a running program; drafts and prereleases are never "latest";
0.1.1 and 0.1.2 programs read and write a library that 0.1.3 runs; both upgrade in place.
After this pass's change to the library (one added column) the released 0.1.1, 0.1.2 and 0.1.3
were run again beside and in place of this build: see "Old and new on one library" in
docs/updating.md.

## The page

| # | What | How to see it | Severity | Status |
|---|---|---|---|---|
| P1 | Removing a note brings the recording behind it into view: the same words reappear as a dictation in History, in search, in exports and in the counts. | Remove one demo note: `folded` 50 to 49, dictations 327 to 328, "Note recording" 1 to 2, a search for the note's words still finds them. | high | fixed in 108221a: removing a note takes the recording behind it along and putting it back returns both (test `removing_a_note_takes_the_recording_behind_it_along`). |
| P2 | CSV exports let a spreadsheet run a formula: text beginning with `=`, `+`, `-` or `@` is written as it is. | Export an item whose text is `=HYPERLINK("http://evil","x")` as CSV and open it in a spreadsheet. | medium | fixed in bb4561b: in CSV, an id, kind, source, title or text that begins with `=`, `+`, `-`, `@`, a tab or a return gets an apostrophe in front. The command line's CSV is the same writer. |
| P3 | On Notes, the heading ("56 open, 0 done") does not change when a note is ticked, while the tab's count does. | Tick one note: tab says 55, heading still says 56 open, 0 done. | medium | fixed in d829083. |
| P4 | An open meeting does not change when the phone sends the finished transcript. | Open a meeting that is still transcribing, send it again with a title and text: six seconds later the page still shows the empty one. | medium | fixed in ed9ec43: the open item is asked for again when the library changes and drawn again only when it differs. Seen: an empty meeting became "Roadmap sync" with its turns within seven seconds. The same commit fixes a second fault found on the way: a change that arrived while something was selected, a dialog was open or a phone was being renamed was never drawn afterwards. |
| P5 | History's counts beside the kinds do not follow "Show them": with the recordings shown the heading says 471 items and "Everything" still says 421. | `#/history?fold=0`. | medium | fixed in 8120a61. |
| P6 | A link or a remembered filter that names a phone which is no longer paired breaks History: "Could not load this. no such paired phone. Is zone still running?" | `#/history?phone=<an unpaired id>`. | medium | fixed in edf582f. |
| P7 | The command line and agents list and export the recordings behind notes; the page leaves them out. One library, two counts. | `zone export --format json` has 51 `source: note` items where the page's export has 1. | low | partly: `zone status --json` and `sync_status` now say how many there are (`items.recordings_behind_notes`, 6acd79b), and Zone for Mac's menu leaves them out as the page does (a7c2163). Left: `zone list`, `zone export` and the agents' tools still list them, on purpose (the agents' instructions describe source `note` as the raw dictation behind a voice note). |
| P8 | An empty library's statistics say "Time spoken 1 s". | Statistics on an empty library. | low | fixed in 44ba642. |
| P9 | Pressing `e` on Settings opens an export of whatever History last showed. | Settings, press `e`. | low | fixed in 44ba642. |
| P10 | Markdown exports carry the words as they are, so a note that contains HTML is HTML to a Markdown viewer that allows it. | Export a note containing `<img src=x onerror=...>` as Markdown. | low | left: escaping would change the person's words in a file meant to be read as text, and Markdown viewers that render raw HTML from a local file are the reader's choice. The page itself never renders words as HTML (checked below). |
| P11 | An item whose id contains a comma cannot be exported or opened by id (ids are sent comma separated). | A fake phone sending id `A,B`. | low | left: the phone's ids are UUIDs. |
| P12 | A change that arrived while something was selected, a dialog was open or a phone was being renamed was never drawn: the page took note of it and then forgot. The list stayed as it was until the next change. | Select a row in History, send an item from the phone, clear the selection, wait. | medium | fixed in ed9ec43 (seen: the new item is listed six seconds after the selection is cleared, and the selection held while it arrived). |
| P13 | A recording that zone 0.1.3 left in view when its note was removed (P1) stayed in view after updating. | Remove a note with the released 0.1.3, then run this build on the library. | medium | fixed in a4a8a35: it goes with its note when the library opens, and comes back with it. |
| P14 | A hand-written link with a day that is not a day (`from=2026-13-45`), days the wrong way round, or `done=maybe` gave "Could not load this" or an empty list. | `#/history?from=2026-11-17&to=2026-11-10`. | low | fixed in bb4ca00. |
| P15 | A meeting's title with a line break in it broke the heading of a Markdown or text export in two. A speaker's name likewise. | A fake phone sending a title of two lines. | low | fixed in a7a6fb8. |
| P16 | In a meeting that arrived without segments, a sentence with a colon ("So the plan is: we go Friday") was taken for a speaker named "So the plan is". | Unit test. | low | fixed in 6237f5e: a name is three words at most and has no comma or question mark. |
| P17 | A meeting with no transcript yet opened as a heading over nothing. | A fake phone sending a meeting with empty text. | low | fixed in ed9ec43: it says no transcript has arrived yet. |
| P18 | Exports are built whole in memory, not written as they go. | `zone export --format json` on 100,000 items: 65 MB written in 2.8 s with a peak of 625 MB (unoptimized build); CSV 1.6 s and 103 MB; Markdown 0.6 s and 162 MB. | low | left: fast enough, and the page refuses more than 50,000 items at once and says to narrow it or use `zone export`. The JSON writer is the one worth making streaming if libraries grow. |
| P19 | With one phone chosen in the bar, the number on the Notes tab still counts every phone's open notes, while the heading beside it counts the chosen phone's. | Two phones, choose one, open Notes. | low | left: the tab's number comes from the status every view shares. Worth doing with the redesign. |
| P20 | An item whose time is in the future (a phone with a wrong clock) is listed under a day that has not come, which the calendar cannot pick. One dated at zero makes "First item: December 31, 1969". | A fake phone sending such items. | low | left: nothing is lost or miscounted (today, the week and the chart leave future days out). |
| P21 | `zone import` and a backup folder say "1 already in the library" for an item that is in fact removed from this computer and was left out. | Remove a note, import a backup that holds it. | low | left: wording. |

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
  when the phone deletes it, and counts never went negative. The same through a backup: an
  import that holds the removed note as it was leaves it removed, one that holds a later edit
  brings it back (with its recording, folded). Flash goes through the same code and was not
  shown a phone's screen in this pass (the unit test `a_removed_item_stays_away_until_the_phone_edits_it`
  covers all three carriers). Several at once: three removed with two ids that do not exist,
  three put back, the counts right each time, and the selection's Remove with its Undo on the
  page. After thirty days: the words are erased at the next look (the row in `removed` holds
  nothing), Put back does nothing, the phone sending the item again in the same version does
  not bring it back, and an edit on the phone does. Search, statistics, exports, `zone search`,
  `zone export` and the agents' `search_items` all leave a removed note and its recording out.
- **The recordings behind notes**: the page's home, History, Statistics and exports count a
  voice note once and agree with each other (117, 117, 117 and 117 in the test library, 128
  with "Show them" and in the command line). A search finds the note, not the recording behind
  it. The numbers beside History's kinds now follow "Show them" and the phone chosen (P5).
- **Time**: on the day the clocks go back (25 hours, 2026-11-01 in Los Angeles) the day holds
  its first moment, both half past ones and its last minute, and neither neighbour's; the same
  on the 23 hour day in March. "Today", "7 days" and a range picked on the calendar agree with
  the statistics (17 and 107 in the test library). The chart's last bar is today. Days are the
  computer's, whatever zone the phone was in: an item carries a moment, not a wall clock.
  A range across a month and across the clock change counts right. A day in the future cannot
  be picked.
- **Meetings**: without `segments` the lines of the text are the turns; a turn without a
  speaker is shown without one; a 20,000 character turn and a 300 character name are shown
  and exported whole; a meeting sent again with its transcript replaces the empty one (P4).
- **Exports**: Markdown, plain text, CSV and JSON for notes alone, one meeting, a range of
  days and a selection: the JSON parses, the CSV has nine columns in every row with commas,
  quotes and line breaks inside fields, and no cell begins with a character a spreadsheet
  would run (P2). A note that looks like Markdown stays inside its list item.
- **Dialogs and keys**: with Export open, `j`, `x` and `s` do nothing to the list behind it,
  Tab stays in the dialog, Escape closes it. `?` opens the list of keys.
- **An empty library** shows the welcome on home and says so on every view. Unknown routes go
  home.
- **Phones**: a nickname survives the phone renaming itself; two phones with one name are
  told apart by id on the page; unpairing keeps the items and re-pairing the same phone gets
  them back under its filter.
- **Keys**: letters typed into the search field or a rename field are text, not shortcuts.
  Back and forward walk the routes.
- **Hostile routes**: unknown routes fall back to home, unknown ids say the item is not here.
- **Speed at 100,000 items**: home loads in 2.3 s in headless Chrome, statistics in 0.34 s,
  History in 0.16 s, a search in 0.16 s; `/api/status` 0.33 s, `/api/stats` 0.17 s. Measured
  again after this pass's changes (an unoptimized build): statistics 0.24 s, History 0.48 s
  with the new counts beside the kinds, Notes 0.64 s, `/api/status` 0.31 s.

## Zone for Mac

| # | What | How to see it | Severity | Status |
|---|---|---|---|---|
| M1 | The last meeting sits at the top of the popover as a bare title. Only the tooltip and VoiceOver say "Last meeting". | `mac/Zone/PopoverView.swift`, `pinned`. | medium | fixed in 4997016: the words "Last meeting" stand before the title, and a meeting without a title is "Untitled" there (seen in the app's sandbox). |
| M2 | The menu listed and counted the recording behind a voice note as a dictation of its own, so the same words were there twice and "Dictations 106" stood where the page says 94. | The app's sandbox with the demo library. | medium | fixed in a7c2163 (with 6acd79b): All 123, Dictations 94, as on the page. |

Checked and sound in the app: items removed on the page are gone from the menu (it reads them
with `zone list`, which leaves them out); the open notes are the ones not done, as on the page;
the clip card leads for fifteen minutes from when the clip arrived, and names when it was sent;
a zone older than the app is replaced at launch and one newer is kept (U6); `swift test` in
`mac/ZoneKit` passes (122 tests).

## Not verified

- **Windows**: `install.ps1`, replacing a running `zone.exe`, and the restart through the
  launcher were read, not run. The Windows job in CI runs the script's test.
- **Linux**: the restart through systemd, and a running zone starting the version that
  replaced it, were not run on Linux (the code is the same as on macOS and uses nothing of
  macOS; CI runs the test there).
- **A real update from this build to a later release**: there is no later release. A copy of
  this checkout built as 0.1.4 stood in, and the test suite's stand-in release server.
- **Flash by camera** for a removed item: unit test only.
- **The page inside Zone for Mac's History window** was not driven by hand after the fixes; it
  is the same page, and the app's own check of it is part of its build.
- **The signed app**: the app was built unsigned into a folder of its own for the sandbox.
  `mac/build` (the notarized 0.1.3 disk image) was not touched, so the fixes to the app are in
  the source and not in that image.
- **The 0.1.2 app in people's hands** (its Update button) was not run: its source was read.
