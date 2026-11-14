# zone

The computer side of Zone Link. One small program that receives the library of the Zone iPhone
app (dictations, voice notes, meeting transcripts), keeps a copy in SQLite on your computer, and
serves it to you and to AI agents.

- **For you:** a local web page and a command line to read, search and copy what you said.
- **For agents:** an MCP server with six tools, so Claude Code, Claude Desktop, Codex or Cursor can
  answer "what did I say about the lease on Wednesday afternoon" from your own words.
- **No account and no Zone server.** The phone talks to your computer directly, or through an iroh
  relay that only forwards encrypted packets.

The wire format between phone and computer is in [PROTOCOL.md](PROTOCOL.md).

## What works today, and what does not

| | State |
| --- | --- |
| Link (iroh), computer side | Built. Tested against a stand-in phone over real iroh: direct, relay only, and found by endpoint id alone. |
| Backup folder import | Built. Tested on synthetic backups and on one real Zone backup. |
| MCP server, command line, web page | Built and tested on macOS. |
| Flash (animated QR, carrier 2) | **Not built.** `/flash` on the web page answers "not built yet". Only its key is derived and kept at pairing, so phones paired now will not have to pair again when Flash arrives. |
| A real iPhone | **Not tested.** The iPhone side of Link is being built separately. Until both meet, treat pairing with a real phone as unproven. |
| Windows and Linux | The program compiles for both (cross-compiled from macOS). It has **not been run** on either, so `zone install` there is untested. |

## Install

You need Rust 1.91 or newer.

```sh
# in a checkout of this repository
cargo build --release
cp target/release/zone ~/.local/bin/      # or anywhere on your PATH
```

Then start it at login (no admin rights needed) and check it:

```sh
zone install     # a launchd user agent on macOS, a systemd user unit on Linux, a per-user Run entry on Windows
zone doctor      # checks the setup and says how to fix anything that is off
```

To run it by hand instead: `zone serve`.

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
3. **Flash.** The phone shows a fast sequence of QR codes and the computer's camera reads them. No
   network at all. Not built yet.

The merge rule: a new id is inserted; an existing id is replaced when the incoming version is as
new or newer; a deletion on the phone keeps the row and drops its text. Items the phone merely aged
out of its own history are kept here: the computer is the long-term copy.

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

zone speaks MCP over stdio. Register it with a client:

```sh
zone mcp install claude-code       # runs: claude mcp add --scope user zone -- /path/to/zone mcp
zone mcp install claude-desktop    # edits claude_desktop_config.json
zone mcp install codex             # edits ~/.codex/config.toml
zone mcp install cursor            # edits ~/.cursor/mcp.json
zone mcp status                    # where it is registered
zone mcp uninstall cursor
```

Only the `zone` entry is added or removed. Everything else in those files is kept, and a copy of
the original is saved beside it as `<file>.before-zone` the first time. Add `--read-only` to
register a server that cannot flag items. For any other client, the command to run is
`/path/to/zone mcp`.

The server opens the database itself, so it works whether or not `zone serve` is running.

### The tools

Deliberately few.

| Tool | What it does |
| --- | --- |
| `list_items(start?, end?, kinds?, sources?, order?, limit?, cursor?, unprocessed_only?, timezone?)` | Short cards for a time range (id, kind, source, local time with weekday and UTC offset, duration, word count, first 200 characters, processed flag), the total that match, counts by kind, and a cursor for the next page. |
| `search_items(query, start?, end?, kinds?, sources?, limit?, timezone?)` | Full-text search with snippets around the matches. Says when results were cut. |
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
computer's iroh identity), `web.token` (the web page's key), `zone.log`, and `daemon.json` while
zone is running.

Other settings, all optional: `ZONE_PORT` (web page port, default 47813), `ZONE_NAME` (the name
shown to the phone), `ZONE_RELAY_URL` (use your own iroh relay instead of the public ones; it is
then included in the pairing link).

## Uninstall

```sh
zone mcp uninstall claude-code    # and any other client you registered
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

iroh is pinned to the 1.4 line, the same one the phone's Swift binding (`iroh-ffi` 1.2.0) uses.
Endpoint ids are written the way iroh 1.x prints them: 64 lowercase hexadecimal characters.

## License

MIT. See [LICENSE](LICENSE).
