# Clips on the command line

A clip is text the person sent from Zone on the iPhone to this computer on purpose: what was on
the phone's clipboard, something shared from another app, a shortcut, or something typed. The
contract between phone and computer is the "Clips" section of [PROTOCOL.md](../PROTOCOL.md). This
page is the computer's side as other programs see it: the commands, their exit codes and the exact
JSON. Names and meanings here are stable. Fields may be added, none will be renamed or removed.

A clip is an ordinary item of kind `clip`, so everything that reads items reads clips too:
`zone list --kind clip --json`, `zone search`, `zone show <id>`, `zone status --json`
(`items.by_kind.clip`, absent until there is one), the web page and the MCP tools
(`kinds: ["clip"]`).

## `zone clip`

Prints the newest clip's text and nothing else. The newest is the one with the latest
`created_at` (when the person sent it), not the one that arrived last.

- To a pipe or a file the text is written exactly, with no line end added. To a terminal a line
  end follows it.
- `--copy` also puts the text on this computer's clipboard.
- `--id <ID>` prints that clip instead of the newest. `ID` is an id from `zone clip list`, or the
  beginning of one.
- `--json` prints the object below instead of the text.

Exit codes: 0 printed, 2 there is no clip (nothing on standard output, one line on standard
error), 1 anything else, including `--copy` when the clipboard could not be written (the text is
still printed). With `--json` and without `--copy`, "there is no clip" exits 0 and prints
`{"clip": null}`.

### `zone clip --json`

```json
{
  "clip": {
    "id": "7D8207BD-9E41-437C-A06B-F8D756C0C06F",
    "source": "clipboard",
    "created_at": 1794787285.518433,
    "created_local": "2026-11-15T16:01:25-08:00",
    "received_at": 1794787285.518979,
    "age_s": 2.0,
    "carrier": "link",
    "device_id": "BBE54906-5FBB-404F-95F4-0EF6760EEA47",
    "length": 45,
    "bytes": 45,
    "lines": 2,
    "first_line": "https://example.com/tickets?id=42",
    "text": "https://example.com/tickets?id=42\nsecond line"
  }
}
```

When there is no clip: `{"clip": null}`.

| Field | Meaning |
| --- | --- |
| `id` | The item's id, uppercase. Works with `zone clip --id` and `zone show`. |
| `source` | How it was sent: `clipboard`, `share`, `shortcut` or `typed`. Passed through from the phone, so another value or null is possible. |
| `created_at` | When the person sent it, Unix seconds. |
| `created_local` | The same moment as RFC 3339 in the computer's time zone. |
| `received_at` | When this computer stored it, Unix seconds. |
| `age_s` | Whole seconds since `created_at`, never below 0. |
| `carrier` | How it reached this computer: `link` or `flash`. |
| `device_id` | The sending phone's install id, or null. |
| `length` | Characters (Unicode scalar values). |
| `bytes` | Bytes of UTF-8. At most 262144 (256 kB): a larger clip is not stored. |
| `lines` | Number of lines. |
| `first_line` | The first line that is not blank, on one line, cut to 80 characters with `…` when longer. |
| `text` | The whole text. Only here, never in a list. |

### `zone clip --copy --json`

The same object with three more fields beside `clip`:

```json
{
  "clip": { "id": "...", "text": "..." },
  "copied": true,
  "tool": "pbcopy",
  "error": null
}
```

- `copied`: whether the text is now on the clipboard.
- `tool`: what wrote it: `pbcopy`, `clip.exe`, `Set-Clipboard`, `wl-copy`, `xclip`, `xsel`, or
  `ZONE_CLIPBOARD_COMMAND`. Null when not copied.
- `error`: why not, as a sentence, or null. Never contains the clip's text.

Exit 0 when copied, 1 when not, 2 when there was no clip to copy (`"clip": null`,
`"copied": false`).

## `zone clip list`

Recent clips as short cards, newest first. `--limit N` (default 20). `--json`:

```json
{
  "total": 2,
  "clips": [
    {
      "id": "7D8207BD-9E41-437C-A06B-F8D756C0C06F",
      "source": "clipboard",
      "created_at": 1794787285.518433,
      "created_local": "2026-11-15T16:01:25-08:00",
      "received_at": 1794787285.518979,
      "age_s": 2.0,
      "carrier": "link",
      "device_id": "BBE54906-5FBB-404F-95F4-0EF6760EEA47",
      "length": 45,
      "bytes": 45,
      "lines": 2,
      "first_line": "https://example.com/tickets?id=42"
    }
  ]
}
```

`total` is how many clips the library holds, `clips` at most `--limit` of them. A card has
exactly the fields of `zone clip --json` without `text`. With no clips: `{"total": 0, "clips": []}`.
Always exits 0.

As text, two lines a clip:

```
Sun 2026-11-15 16:01  clipboard  45 characters  2 lines  7D8207BD
    https://example.com/tickets?id=42
```

## `zone clipboard on | off | status`

Whether a clip goes on this computer's clipboard by itself as it arrives. Off until the person
turns it on. The setting is kept in the library, and the running daemon reads it for each clip, so
a change takes effect at once, with no restart.

With it on, the daemon copies a clip only when all of these hold:

- it came over Link (never from a Flash),
- its `created_at` is less than 120 seconds before the computer's clock (and at most 30 seconds
  after it, for a phone whose clock runs a little ahead),
- it is new to this library (a clip sent again is not copied again),
- it is the newest such clip in its batch.

Every other clip is stored and the clipboard is left alone.

All three print the same object with `--json`, describing the setting after the command:

```json
{
  "clipboard": "on",
  "enabled": true,
  "available": true,
  "tool": "pbcopy",
  "fix": null,
  "max_age_s": 120.0,
  "daemon_running": true
}
```

| Field | Meaning |
| --- | --- |
| `clipboard` | `on` or `off`. |
| `enabled` | The same as a boolean. |
| `available` | Whether a clipboard tool was found, so copying can work. |
| `tool` | The tool that would be used (see `--copy` above), or null. |
| `fix` | When `available` is false: a sentence saying what to install. Otherwise null. |
| `max_age_s` | How new a clip must be to be copied by itself: 120. |
| `daemon_running` | Whether zone is running. Nothing arrives, and nothing is copied, while it is not. |

`zone clipboard on` turns the setting on even when no tool was found (`available` false), so the
answer is worth reading. Always exits 0 unless the library cannot be opened. `tool` is what the
command itself found: a daemon started at login has a shorter `PATH` and, on Linux, may not have
the desktop session's `WAYLAND_DISPLAY` or `DISPLAY`.

### Clipboard tools

The text goes to the tool's standard input. Tried in this order, the first that works wins:

| System | Tools |
| --- | --- |
| macOS | `pbcopy` (found on `PATH`, else `/usr/bin/pbcopy`) |
| Windows | `clip.exe` (given UTF-16 with a byte order mark), then PowerShell `Set-Clipboard` |
| Linux | `wl-copy` (Wayland), `xclip -selection clipboard`, `xsel --clipboard --input` (X11), then `clip.exe` (inside WSL) |

`ZONE_CLIPBOARD_COMMAND` replaces them all: a command line run by `sh -c` (on Windows `cmd /C`)
that gets the text as UTF-8 on its standard input, for example `tmux load-buffer -`, or in a test
`cat > /tmp/clipboard.txt`. It must be set for the daemon to affect clips as they arrive, and for
`zone clip --copy` to affect that. `tool` is then `ZONE_CLIPBOARD_COMMAND`.

## The `clip` event

`zone events --json` (and `--follow`) prints one line for each clip that arrives and is new to the
library, by any carrier, before the `link_session` or `flash_received` line of the session that
brought it. It holds the clip's length and source, never its text.

```json
{"seq":4,"at":1794787285.54,"time":"2026-11-15T16:01:25-08:00","event":"clip","id":"7D8207BD-9E41-437C-A06B-F8D756C0C06F","device":"Test iPhone","device_id":"6e7a67adcbfb3232856541f82dfcf75543117b32f8b4d80a9ccd9954bbc1e144","carrier":"link","source":"clipboard","length":45,"bytes":45,"copied":true,"not_copied":null,"tool":"pbcopy"}
```

| Field | Meaning |
| --- | --- |
| `seq`, `at`, `time`, `event` | As on every event. `event` is `clip`. |
| `id` | The clip's id: `zone clip --id <id> --json` returns it. |
| `device` | The phone's name, or null. |
| `device_id` | The phone's id among the paired phones (as in `zone devices --json`, `endpoint_id`), or null. |
| `carrier` | `link` or `flash`. |
| `source` | `clipboard`, `share`, `shortcut`, `typed`, or null. |
| `length`, `bytes` | Characters and bytes of UTF-8. |
| `copied` | Whether the daemon put it on the clipboard as it arrived. |
| `not_copied` | Why not, or null when copied: `off` (the setting is off), `carrier` (it did not come over Link), `old` (not sent in the last 120 seconds), `newer_clip` (a newer clip came in the same batch), `failed` (no clipboard tool worked: the log says why). |
| `tool` | What wrote the clipboard, or null. |

As text (`zone events`): `Test iPhone sent a clip: 45 characters from its clipboard, now on this computer's clipboard`.

## Agents

- MCP: `kinds: ["clip"]` works in `list_items`, `search_items` and `library_stats`. The newest
  clip (`list_items` with `kinds: ["clip"]`, `limit: 1`) is "the thing I just sent you". The
  server's instructions say so.
- Limited views count clips as a fourth kind. `zone mcp --kinds dictation,note,meeting`,
  `--kinds note,meeting` and `--notes-only` all leave clips out. `--kinds clip` shows only clips.
  `--exclude-sources share` hides clips shared from other apps, the same way `keyboard` hides
  dictations typed by voice.
- `zone mcp status --json` lists `clip` among `scope.kinds` for a client that sees everything:
  `["dictation", "note", "meeting", "clip"]`. A client registered earlier with
  `--kinds dictation,note,meeting` keeps exactly those three and does not start seeing clips.

## Testing against it

`zone dev fake-phone` plays the phone:

```sh
zone dev fake-phone --identity phone.json --addr 127.0.0.1:PORT --clip "some text"
zone dev fake-phone --identity phone.json --addr 127.0.0.1:PORT --clip "from last week" --clip-age 600
zone dev fake-phone --identity phone.json --addr 127.0.0.1:PORT --clip "shared" --clip-source share
```

`--clip TEXT` (repeatable) sends each text as a clip with a new id, `--clip-source` sets the
source (default `clipboard`) and `--clip-age SECONDS` says how long ago it was sent (default 0, so
it counts as just sent). The report it prints has the new ids under `clips`. Pair first with
`--url <pairing link>`, as for any other fake phone session.
