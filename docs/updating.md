# How an update reaches a person

Checked on 2026-11-18 by running each path, with the published 0.1.1 and 0.1.3 programs (and
the 0.1.2 that Zone for Mac carried), each with `ZONE_HOME` and the install folder pointed at
throwaway folders and the page on ports other than 47813. Nothing here touched a real install.

The short version: every way of installing zone ends with the newest release on disk, checked
against that release's `SHA256SUMS`. What was weak was everything after the file is on disk: a
zone that was already running kept running the old program, nothing said so, a page left open
kept showing the old page, and nobody was told a newer release existed unless they went
looking. The fixes are listed at the end with their commits.

## The paths

| Path | What it does | How soon a person has the new version | Gaps |
|---|---|---|---|
| `zone update` | Asks `api.github.com/repos/zaydiscold/zone-mcp/releases/latest` once (6 seconds at most), then runs the install script built into the program you already have, with that version and the folder zone is in. | As soon as they run it. 0.1.1 to 0.1.3 took a few seconds. | Uses the OLD program's copy of the script, so a fix to `install.sh` only helps the update after next. At GitHub's hourly limit for the API (60 questions per address) it stopped with "API rate limit exceeded" even though the download itself needs no API: fixed (7a2df1c), it now asks the website when the API will not say. |
| `zone update --check` | The same question, installs nothing. Not cached: asks every time. | n/a | None found. Offline: says so, exit 1. Installed newer than the latest release: says so, exit 0. |
| `install.sh` one-liner | Fetched from `raw.githubusercontent.com/.../main/`, which GitHub caches for 5 minutes (`cache-control: max-age=300`). Finds the latest by following `github.com/.../releases/latest`, which only ever leads to a published release that is not a draft or a prerelease. Downloads, checks the SHA-256, runs `zone --version` on the download before replacing anything. | As soon as they run it. The script itself can be up to 5 minutes behind `main`; the version it installs is worked out when it runs, so it is never behind. | Re-run over an install: the script restarts zone only when the login item (`zone install`) points at this very file. A `zone serve` in a terminal, or one Zone for Mac started, kept running the old program: fixed, a running zone now starts the version that replaced it. See "After the file is replaced". The script also removed the old `zone` before moving the new one in, leaving a moment with nothing at the path: fixed (abc93b5), one move. |
| `install.ps1` one-liner | The same on Windows. A running `zone.exe` is moved aside (Windows allows a rename, not an overwrite), the new one takes its name, the background zone is stopped and started again through its launcher. | As soon as they run it. | Read and covered by the Windows CI job only. Not run by hand for this review (no Windows machine in the loop). |
| Homebrew | There is no tap. `zaydiscold/homebrew-zone` does not exist (404). Each release carries a `zone.rb` with that release's version and checksums (0.1.1 and 0.1.3 checked against `SHA256SUMS`: they match). | Never, until a tap is published and its `Formula/zone.rb` is replaced at each release. | README says "soon", which is true. `brew upgrade` will lag every release by however long the tap's copy takes to be replaced: that step is by hand today. |
| `cargo install --git` | Builds `main` at the moment it is run, not the latest release. `zone update` refuses to replace it and prints the command to run again (with `--force`). | When they run it again. | README gives the first-install line; the update line needs `--force`, which `zone update` prints. |
| Zone for Mac | The app carries a zone inside and copies it to `~/Library/Application Support/Zone/bin/zone` at launch when the two differ. Agents and the `zone` command point at that copy. The app checks GitHub when its Settings come into view (at most every 15 minutes) and shows "0.1.x is available" with an Update button. | When they download a new `Zone.dmg` and replace the app. | Three, below. |
| The page in a browser | Every answer is `cache-control: no-store`, fonts included, so nothing is cached across versions. | A reload after zone restarts. | An open tab never noticed that zone had been replaced: it kept running the old page's script against the new zone until someone reloaded. Fixed: the page reloads itself. |
| MCP (`zone mcp` started by an agent) | The agent's settings name a path. Every way of updating replaces the file at the same path, so the next session starts the new version. | The agent's next session. | A session that is already open keeps its old `zone mcp` until it ends. That is safe: see "Old and new on one library". |

## After the file is replaced

Run on 2026-11-18: a 0.1.1 zone serving on a throwaway port, `zone update` in its folder.

```
Installed zone 0.1.3 in .../u1/bin/zone
If zone is running (`zone status` says), stop it and start it again to run the new version.
```

The file was 0.1.3, the running zone was still 0.1.1 (same process id as before), and
`zone status` said only "Zone is running". Nothing anywhere named the version that was running.

What restarts by itself:

- A zone started at login by `zone install`: yes. The script asks launchd (macOS) or systemd
  (Linux) to start it again, and on Windows stops it and runs its launcher. It does this only
  when the login item names the very file that was replaced.
- A `zone serve` typed into a terminal: no. It cannot be restarted from outside the terminal.
- A zone that Zone for Mac started: no. The app restarts its own zone only when the app itself
  is replaced and opened again.

Fixed in this pass, two ways.

First, zone restarts itself. On macOS and Linux a running zone looks at its own program file
every ten seconds. When another version of zone has taken its place (and that program runs:
it is asked for its version first), zone cleans up as it does when stopped and starts the new
program in its place, as the same process with the same arguments. So the three cases above
all end with the new version running within about fifteen seconds, whoever started zone. Tried
with a 0.1.3 build and a stand-in 0.1.4: in a terminal, and as the zone Zone for Mac started
(same process id before and after, the app noticed nothing). The same version put there again
changes nothing. On Windows the installer stops and starts zone as before.

Second, it is said. `zone status` and `zone doctor` name the version that is running when it
is not the version of the program on disk, `zone status --json` carries it as `daemon.version`,
Zone for Mac shows it at the foot of its Settings, and an open page loads the new page when
zone comes back as another version (four seconds, in the same test).

This reaches people from the release after 0.1.3. A zone 0.1.1 to 0.1.3 that is running when
it is updated still has to be stopped and started once, unless it starts at login.

## Who is told that a newer release exists

Before this pass, only someone who ran `zone update`, ran `zone doctor`, opened Settings on the
page, or opened Settings in Zone for Mac. A person who installed 0.1.1, paired, and has used
the page's home view and their agents since would never hear of 0.1.3.

- The page asks through zone itself, and zone keeps the answer for 12 hours. So for up to 12
  hours after a release, Settings can say "Up to date". Restarting zone empties that memory.
- Now: the page asks when it is opened (the same question, the same 12 hours, still off with
  `ZONE_NO_UPDATE_CHECK=1`), and says once per version that a newer one is out. This widens
  what README promised ("Opening its Settings lets zone ask GitHub"): it is now "having the
  page open". The owner should confirm that is wanted; it is one small commit to take back.
- zone still never asks while nobody has the page open, and the command line still asks only
  when `zone update` or `zone doctor` is run.

## Zone for Mac

1. **The Update button led to a page with nothing for the app on it.** It opens the release's
   page. v0.1.3 has four command line archives and no `Zone.dmg` (neither did 0.1.0 or 0.1.1),
   so a person pressing Update gets a list of files none of which is the app. Fixed in the
   app's source: the button is offered only when the release carries a `.dmg`, and then it
   downloads that file. Without one the line says the app for that version is not out yet.
   This reaches people only with the next app build. For today: attach `mac/build/Zone.dmg`
   (0.1.3, signed and notarized) to the v0.1.3 release.
2. **Two copies of zone can disagree.** Seen on the owner's Mac during this review: the zone
   running was `~/.local/bin/zone` 0.1.3 (started at login by `zone install`), while the app's
   own copy was 0.1.2. The app then reads the library through its 0.1.2 program while 0.1.3
   runs it. That works (see the next section), but the app showed its own copy's version and
   gave no sign that another version was running. Fixed: the foot of Settings names the
   version that is running when it differs.
3. **Opening the app could put an older zone back.** `zone update` run through the app's
   `zone` command replaces the app's copy (the path is not inside the app, so zone treats it
   as an install by script). The next time the app opened, it saw a copy that differed from
   the one inside it and replaced it, with the older one. Fixed: the app leaves a newer copy
   alone.

The app does not update itself, and nothing in this pass changes that: a new app is a new
`Zone.dmg`, downloaded and dragged to Applications.

## Old and new on one library

A long agent session keeps its old `zone mcp` after an update, and the Mac case above has a
0.1.2 command line reading what 0.1.3 runs. Both were tried, with libraries made by the real
0.1.1 and 0.1.2 programs and then opened by a 0.1.3 zone:

| | 0.1.1 | 0.1.2 |
|---|---|---|
| Library opened in place by 0.1.3 | Works. One table added (`removed`), three columns on `devices`, nothing rewritten, `user_version` stays 1. | Same. |
| Old `zone status`, `list`, `search`, `show`, `stats` on it | Work. An item removed on the 0.1.3 page is left out of all of them. | Same. |
| Old `zone mcp`: `list_items`, `mark_processed` | Work. A mark set by the old program shows as Done on the 0.1.3 page. A removed item is "not found". | Same. |
| Going back: the old zone runs the 0.1.3 library | Works. One flaw: when the phone sends everything again under the old zone, an item that was removed on the page comes back (the old zone does not know about removal). Back on 0.1.3 that item was both in the library and in "Removed from this computer", where Put back did nothing. Fixed: when a zone with the fix opens the library, such an item is removed again (it can be put back as before), unless the phone edited it in the meantime, in which case it is back as the phone has it. | Same. |

## What was fixed, and where

See `docs/reviews/2026-11-18-logic-pass.md` for the commits. In short: the running version is
said out loud, the page reloads itself, the page tells of a newer release, `zone update`
survives the API limit, the install script never leaves a moment without a `zone` at its path,
the Mac app offers only what it can deliver and never puts an older zone back.

## Still open

- **Attach `Zone.dmg` to v0.1.3** (owner). Until then the 0.1.2 app's Update button leads to a
  page without the app.
- **A Homebrew tap** (owner): create `zaydiscold/homebrew-zone`, and replace its
  `Formula/zone.rb` with the release's `zone.rb` at every release (worth a step in the release
  workflow, which would need a token for the tap).
- **Everyone on 0.1.1 to 0.1.3 keeps those versions' behaviour** until they update once: their
  `zone update` runs the script built into the program they have, their page does not reload
  itself, and their zone does not mention a newer release outside Settings. Only a release
  note, or the phone app saying so, reaches them.
- **The download is checked against a checksum that comes from the same release.** That
  catches a broken or swapped file, not a release that was replaced whole. Signing the
  checksums would close that; not done here.
- **Windows**: a `zone serve` typed into a terminal there is still not restarted by an update
  (a running `.exe` cannot be swapped for another in place). The login item is, by the
  installer.
