# zone

The computer side of Zone Link. One small program that receives the library of the Zone iPhone
app (dictations, voice notes, meeting transcripts), keeps a copy in SQLite on your computer, and
serves it to you and to AI agents.

- **For you:** a local web page and a command line to read, search and copy what you said.
- **For agents:** an MCP server with six tools, registered with every AI client on the computer
  (Claude Code, Codex, Cursor, Hermes, Gemini CLI, Zed and fourteen more), so any of them can answer
  "what did I say about the lease on Wednesday afternoon" from your own words.
- **No account and no Zone server.** The phone talks to your computer directly, or through an iroh
  relay that only forwards encrypted packets.

The wire format between phone and computer is in [PROTOCOL.md](PROTOCOL.md).

## What works today, and what does not

| | State |
| --- | --- |
| Link (iroh), computer side | Built. Tested against a stand-in phone over real iroh: direct, relay only, and found by endpoint id alone. |
| Backup folder import | Built. Tested on synthetic backups and on one real Zone backup. |
| MCP server, command line, web page | Built and tested on macOS. |
| Flash (QR codes from the phone's screen to the computer's camera) | Built, computer side. Tested with pictures of the codes: ones drawn by this program, ones written by the iPhone app's own code, and a video of them played to a browser as if it were the camera. **Not yet tested with a real phone held up to a real camera**, so how it copes with glare, blur and distance is unknown. |
| A real iPhone | **Not tested.** The iPhone side is being built separately. Until both meet, treat pairing with a real phone as unproven. |
| Windows | Built and run on Windows 10: see [Windows](#windows) for exactly what was checked. |
| Linux | The program compiles for it (cross-compiled from macOS). It has **not been run** there, so `zone install` on Linux is untested. |

## Install

You need Rust 1.91 or newer.

```sh
# in a checkout of this repository
cargo build --release
cp target/release/zone ~/.local/bin/      # or anywhere on your PATH
```

Then start it at login (no admin rights needed) and check it:

```sh
zone install     # a launchd user agent on macOS, a systemd user unit on Linux, a per-user Run entry on Windows,
                 # then registers zone with the AI clients it finds (--no-mcp to skip that)
zone doctor      # checks the setup and says how to fix anything that is off
```

To run it by hand instead: `zone serve`.

### Windows

You need Rust with the MSVC toolchain (what `rustup` installs by default on Windows, together
with the Visual Studio C++ build tools). In PowerShell:

```powershell
# in a checkout of this repository
cargo build --release                      # about four minutes the first time
# copy target\release\zone.exe to any folder on your PATH, then:
zone install                               # start at login, no admin rights needed
zone doctor
```

`zone install` writes a small script, `zone-serve.vbs`, into the data folder
(`%LOCALAPPDATA%\Zone`) and adds a per-user entry named `Zone` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` that runs it at login. The script starts
`zone serve` without opening a window. `zone uninstall` removes both and stops zone.

What was checked on a Windows 10 computer (Rust 1.97, MSVC), and what was not:

- Checked: the build (`zone.exe` is about 7.7 MB), the whole test suite, `zone status`,
  `zone doctor`, `zone serve`, `zone pair`, `zone stats`, `zone search`, and reading Flash pictures
  with `zone dev flash-decode`.
- Checked across two computers: a stand-in phone on a Mac paired with the Windows computer and sent
  it items, once the normal way (it began on the relay and moved to a direct connection) and once
  forced through the relay only. The counts on Windows matched what was sent.
- Checked: `zone install --no-start` writes the script, and running the script starts zone in the
  background.
- Not checked: the login entry itself (`zone install` without `--no-start`, then logging out and
  in), `zone uninstall`, `zone open` launching a browser, and the Flash page with a Windows camera.

If the web page's usual port (47813) is taken by another program, zone picks a free one and
`zone status` shows which. `zone open` always uses the right one.

## Pair your iPhone

```sh
zone pair
```

This prints a QR code and a `zone://pair?...` link. Scan the code in Zone's Link settings on the
phone. The code works once and expires after ten minutes. The same code is available in the web
page under "Pair a phone".

```sh
zone devices            # paired phones and when each last synced
zone unpair "My iPhone" # forget one (its items stay)
zone resync             # ask the phone to send everything again at its next sync
```

## The three ways items arrive

All three end in the same merge into the same database, so they can be mixed freely.

1. **Link.** A paired phone connects over [iroh](https://www.iroh.computer) whenever it has something
   new: directly on the same network, by hole punching across networks, or through an iroh relay
   when neither works. Automatic once paired.
2. **Backup folder.** Zone can already keep a backup in iCloud Drive, OneDrive or Dropbox. Point zone
   at it and it re-imports whenever the backup changes. No phone update needed.

   ```sh
   zone backup-folder ~/Library/Mobile\ Documents/com~apple~CloudDocs/Zone\ Backups
   zone import path/to/Zone\ library.zonebackup     # or read one once
   ```
3. **Flash.** The phone shows a fast series of QR codes and the computer's camera reads them. No
   network at all. See the next section.

### When the three disagree

Whatever the phone itself sends, over Link or Flash, wins: it replaces the copy on the computer,
even if the computer's copy looks newer. If you delete something on the phone, the computer keeps
a record that it existed but drops its words, and if you bring it back on the phone it comes back
here too. A backup folder is treated with more care, because a backup can be old: an item from a
backup replaces the computer's copy only when it is at least as new, and a backup never brings
back something you deleted. Things the phone simply stopped keeping because they got old are never
removed here. The computer is the long-term copy.

## Flash: sync with no network

Flash moves your library from the phone's screen to the computer's camera. Use it when the phone
and the computer cannot reach each other at all: a network that blocks everything, a plane, a
computer you keep offline. When Link works, Link is easier, because it is automatic.

1. Run `zone open` and choose **Flash** at the top of the page (or go straight to
   `http://127.0.0.1:47813/flash` once the page is unlocked).
2. Allow the camera when the browser asks. The pictures never leave your computer: the page hands
   them to zone on the same machine, and nothing is loaded from the internet.
3. On the iPhone, start Flash in Zone's computer sync settings and hold the screen up to the
   camera so the code fills most of the square on the page. Turn the phone's brightness up.
4. Watch the bar. The page shows how many codes it has read and how many pieces of the library it
   has out of the total. When it says Done, it also says how many items were new, changed, or
   already here. Then tap Done on the phone.

You do not have to catch every code. Any code helps, in any order, so if the camera misses some
the phone just keeps going until the computer has enough. If you put the phone down early,
nothing is stored and you can start again.

The phone has to be paired with the computer first, because the codes are encrypted with a key
both sides get at pairing. Pairing does not need a network either: scanning the code from
`zone pair` (or "Pair a phone" on the page) is enough, and the first flash then completes the
pairing.

How fast: the phone shows 12 codes a second, each carrying 300 bytes of compressed, encrypted
data. That is about 3.6 kilobytes a second, which for ordinary speech is over a thousand words a
second. A day of dictation takes a few seconds and a library of a thousand items a minute or two.
Those figures come from the code sizes, not from a stopwatch: with a real camera some codes will
be missed, so expect it to take somewhat longer. Reading the codes is not the slow part: zone
reads a camera picture in about a fiftieth of a second.

## See your transcriptions

```sh
zone open
```

opens a local page with everything grouped by day: filter by kind (dictations, notes, meetings) and
by where it was spoken, search, pick a date range, copy any item.

From the terminal:

```sh
zone list --since yesterday
zone list --kind meeting --since 2026-11-01 --until 2026-11-07
zone list --since "2026-11-11 12:00" --until "2026-11-11 17:00" --order oldest
zone search lease renewal --kind dictation --since 30d
zone show 9F1C2E6A            # an id, or the beginning of one
zone stats --by day --since 7d
zone export --format md --since 2026-11-01 -o november.md
zone export --format jsonl > library.jsonl
```

Times can be RFC 3339 (`2026-11-11T12:00:00-08:00`), a date (`2026-11-11`), a date and time
(`"2026-11-11 12:00"`, in the computer's time zone, or the one given with `--tz`), an age (`2h`,
`3d`, `1w`), or `today` / `yesterday`. A bare date as `--until` means through the end of that day.
Output shows local time with the weekday. Add `--json` to any query for scripts.

## Use it from an AI agent

zone speaks MCP over stdio. `zone install` registers it with every AI client it finds on the
computer, so after that an agent can simply use it. To do it yourself, or again later:

```sh
zone mcp install --all           # every client found here (same as: zone mcp install detected)
zone mcp install claude-code zed # or name them
zone mcp status                  # where it is registered, one line per place
zone mcp uninstall cursor        # take it out of one (zone then leaves that one alone)
zone mcp uninstall --all         # take it out of every one
```

`--all` prints one line per client: registered, already registered, skipped (not installed, or
you took zone out of it) or failed, with the reason. A client counts as installed when its
settings folder exists or its command is on your PATH.

| Client | Name | Where zone writes |
| --- | --- | --- |
| Claude Code | `claude-code` | Every profile: `~/.claude.json`, and `.claude.json` in each `CLAUDE_CONFIG_DIR` folder (`~/.claude*`). Through `claude mcp add --scope user` when the command is there, else by editing the file the same way. |
| Claude Desktop | `claude-desktop` | `claude_desktop_config.json` |
| Codex | `codex` | `config.toml` in `~/.codex`, `CODEX_HOME` and any other `~/.codex*` folder that has one |
| Cursor | `cursor` | `~/.cursor/mcp.json` (the editor and the `cursor-agent` command) |
| Hermes | `hermes` | `~/.hermes/config.yaml` (or `HERMES_HOME`) and every profile's `config.yaml` |
| Gemini CLI | `gemini` | `~/.gemini/settings.json` |
| Qwen Code | `qwen` | `~/.qwen/settings.json` |
| opencode | `opencode` | `~/.config/opencode/opencode.json` |
| Windsurf | `windsurf` | `~/.codeium/windsurf/mcp_config.json` |
| VS Code | `vscode` | The user `mcp.json` (also Insiders when present) |
| Zed | `zed` | `settings.json`, under `context_servers` |
| Kiro | `kiro` | `~/.kiro/settings/mcp.json` |
| Factory Droid | `droid` | `~/.factory/mcp.json` |
| Copilot CLI | `copilot` | `~/.copilot/mcp-config.json` (which VS Code's newer agent also reads) |
| LM Studio | `lm-studio` | `~/.lmstudio/mcp.json` |
| Goose | `goose` | `~/.config/goose/config.yaml`, under `extensions` |
| Crush | `crush` | `~/.config/crush/crush.json` |
| Amp | `amp` | `~/.config/amp/settings.json` |
| Cline | `cline` | `~/.cline/data/settings/cline_mcp_settings.json` (and the older VS Code extension's file) |
| Roo Code | `roo` | The VS Code extension's `mcp_settings.json` |

Paths are the macOS and Linux ones; on Windows each client's own folder is used. Registering with
every profile of Claude Code, Codex and Hermes is also what makes zone available in apps that run
those agents with their own profiles, such as T3 Code.

Only the `zone` entry is added, changed or removed. Everything else in those files stays exactly as
it was, comments included (Zed, VS Code and Hermes keep comments in their settings), a link to a
file kept elsewhere stays a link, and a copy of the original is saved beside it as
`<file>.before-zone` the first time. Add `--read-only` to register a server that cannot flag
items. For any other client, the command to run is `/path/to/zone mcp`.

The server opens the database itself, so it works whether or not `zone serve` is running.

### Staying registered

`zone install` turns on one more thing: when zone starts, it registers with clients installed
since. It only ever adds zone to a client that never had it. It never changes an entry that is
there, and it never puts zone back where you took it out, whether with `zone mcp uninstall` or
by deleting the entry by hand. It does this only when it is the same `zone` program that
`zone install` was run from. `zone install --no-mcp` skips all of it, and `zone uninstall` or
`zone mcp uninstall --all` turns it off. What zone has registered, and where you took it out, is
kept in `mcp-clients.json` in the data folder.

`zone doctor` names any installed client that does not have zone yet and gives the one command
that fixes it.

### The tools

Deliberately few.

| Tool | What it does |
| --- | --- |
| `list_items(start?, end?, kinds?, sources?, order?, limit?, cursor?, unprocessed_only?, timezone?)` | Short cards for a time range (id, kind, source, local time with weekday and UTC offset, duration, word count, first 200 characters, processed flag), the total that match, counts by kind, and a cursor for the next page. |
| `search_items(query, start?, end?, kinds?, sources?, limit?, cursor?, timezone?)` | Full-text search with snippets around the matches, best match first, with a cursor for the next page. |
| `get_items(ids, max_chars?, offset?, timezone?)` | Full text, bounded, with a `truncated` flag and `next_offset`. |
| `library_stats(start?, end?, group_by?, kinds?, sources?, timezone?)` | Counts, words and recorded seconds by local day, kind or source. |
| `sync_status()` | The computer's current local time, weekday and time zone (so an agent can resolve "Wednesday"), paired phones, last sync per carrier, counts. |
| `mark_processed(ids, processed?)` | The only write. Sets or clears a flag so an agent can work through items once. It never changes an item's text. Hidden by `--read-only`. |

The three kinds of item:

- `dictation`: something dictated. Most have source `keyboard`: messages and text typed by voice in
  other apps. Source `app` was dictated inside Zone; source `note` is the raw dictation behind a
  voice note.
- `note`: a voice note saved in Zone.
- `meeting`: a meeting transcript, one `Speaker: words` line per turn, with a title.

## Privacy

- **Your computer keeps a local copy.** It lives in one SQLite file in the data folder below, readable
  only by your user. Nothing is uploaded anywhere by zone.
- **Phone and computer talk directly.** The connection is end to end encrypted between two keys:
  the phone's and the computer's. When a direct path is not possible it goes through an iroh relay,
  which forwards encrypted packets and stores nothing. Finding the computer from its id uses iroh's
  public address lookup, which publishes how to reach the endpoint (its relay), not what it holds.
- **No account, no Zone server.** Pairing is a one-use secret in a QR code. After that the computer
  accepts only the phones it has paired with, by their keys.
- **The web page is local.** It listens on 127.0.0.1 only, needs a random key that `zone open`
  passes to your browser, refuses requests under any other host name, and loads nothing from the
  internet.
- **Agents read what you let them.** An MCP client that has zone registered can read the whole
  library. The one thing it can change is the processed flag.
- **Flash stays in the room.** The camera pictures go from the browser to zone on the same
  computer and are thrown away once read. What the phone shows is encrypted, so someone filming
  the screen gets nothing they can open.
- **Logs never contain your words.** `zone.log` records events and counts only.

One limit worth knowing: the page's key is kept in a cookie for `127.0.0.1`, and browsers do not
separate cookies by port. Another program serving pages on `127.0.0.1` could be sent that cookie if
you visit it in the same browser.

## Where things live

| | Data folder |
| --- | --- |
| macOS | `~/Library/Application Support/Zone` |
| Windows | `%LOCALAPPDATA%\Zone` |
| Linux | `$XDG_DATA_HOME/zone` or `~/.local/share/zone` |

Set `ZONE_HOME` to use another folder. Inside it: `zone.db` (the library), `endpoint.key` (this
computer's iroh identity), `web.token` (the web page's key), `zone.log`, `mcp-clients.json` (which
AI clients zone registered with, and which you took it out of), and `daemon.json` while zone is
running.

Other settings, all optional: `ZONE_PORT` (web page port, default 47813), `ZONE_NAME` (the name
shown to the phone), `ZONE_RELAY_URL` (use your own iroh relay instead of the public ones; it is
then included in the pairing link).

## Uninstall

```sh
zone mcp uninstall --all          # takes zone out of every AI client
zone uninstall                    # stops zone and removes the login item
```

Your library stays in the data folder. Delete that folder to remove it, then delete the `zone`
program.

## Development

```sh
cargo test                        # unit tests and end-to-end tests, all offline
cargo test -- --ignored           # also the tests that use iroh's public relays (needs internet)
ZONE_HOME=$(mktemp -d) scripts/mcp_roundtrip.py --zone target/release/zone
```

Every test runs in a temporary `ZONE_HOME` and a temporary home folder. A hidden test peer,
`zone dev fake-phone`, plays the phone's side of Link over real iroh: it pairs with a link or
reconnects with a saved identity, sends synthetic items or a JSONL file in frames, and prints the
acks. `--relay-only` removes its IP transports so every packet goes through the relay.

Flash can be tested without a phone or a camera. `zone dev flash-encode` draws the codes a phone
would show as numbered PNG pictures, and `zone dev flash-decode` reads a folder of such pictures
back, whatever the files are called and whatever size the codes are drawn at:

```sh
zone dev flash-encode --out /tmp/frames --secret <64 hex characters> --count 200
zone dev flash-decode /tmp/frames --secret <the same 64 hex characters>            # prints counts only
ZONE_HOME=$(mktemp -d) zone dev flash-decode /tmp/frames --secret <...> --import   # also stores the items
```

Without `--secret`, flash-decode tries the keys of the phones paired with this library, the way
the page does. `scripts/flash_reference.py` is the reference for how the codes are made.

iroh is pinned to the 1.4 line, the same one the phone's Swift binding (`iroh-ffi` 1.2.0) uses.
Endpoint ids are written the way iroh 1.x prints them: 64 lowercase hexadecimal characters.

## License

MIT. See [LICENSE](LICENSE).
