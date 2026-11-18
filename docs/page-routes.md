# The library page's addresses

The page at `http://127.0.0.1:47813/` keeps what it shows in the part of the address after `#`.
Every view and every filter has an address, so another program (Zone for Mac's menu, a shortcut,
a script) can open exactly one thing, and the browser's Back and Forward work.

The page is locked without its key. `zone open --print` prints the address with the key
(`/?t=<key>`); the server swaps the key for a cookie and redirects to `/`, and browsers carry
the part after `#` across that redirect. So this opens the notes in a browser that has never
seen the page:

```
http://127.0.0.1:47813/?t=<key>#/notes
```

In a browser that already has the cookie, `http://127.0.0.1:47813/#/notes` is enough.

## Views

| Address | Shows |
| --- | --- |
| `#/` | Home: notes to tick off, meetings, what was said lately, and the figures. |
| `#/notes` | Notes, open ones, newest first. |
| `#/notes?show=done` | Done notes. `show=all` for both. |
| `#/notes?order=oldest` | Oldest first. Without `order`, the order last chosen on this computer. |
| `#/meetings` | Meetings, as cards. |
| `#/meeting/<id>` | One meeting: its transcript by speaker. `<id>` is the item's id. |
| `#/item/<id>` | Any other item in full (a note, a dictation, a clip). |
| `#/history` | Everything, by day. |
| `#/stats` | Statistics. |
| `#/settings` | Settings: appearance, phones, clipboard, backup folder, agents, removed items, version. |
| `#/pair` | Home, with the pairing sheet open and a fresh code in it. |

An address the page does not know shows Home.

## Filters of `#/history`

Joined with `&` after `?`. Leave one out for no limit.

| Parameter | Values |
| --- | --- |
| `q` | Words to search for, as in the search field. |
| `kinds` | One or more of `note`, `meeting`, `dictation`, `clip`, separated by commas. |
| `sources` | Where a dictation was spoken: `keyboard`, `app`, `note`, separated by commas. |
| `done` | `open` (not done) or `done`. |
| `range` | `today`, `7` or `30` (days, today included), or `custom` with `from` and `to`. |
| `from`, `to` | Days as `2026-11-17`, both included. Only read with `range=custom` (or without `range`). |
| `fold` | `0` also lists the recording behind each voice note. Without it a voice note is listed once. |

Examples: `#/history?kinds=meeting,note&range=7`, `#/history?q=ferry&done=open`,
`#/history?from=2026-11-01&to=2026-11-07`.

## The phone

`phone=<id>` on any address shows only what came from one paired phone, and `phone=none` only
items no phone is on record for (read from a backup folder). `phone=all` clears it. The id is
the phone's `id` in `GET /api/status` (its endpoint id, as `zone devices` prints it). The page
remembers the choice on this computer and takes the parameter out of the address, so a later
link without `phone` keeps showing the chosen phone until it is changed in the bar's menu.

## Keys

`/` search, `g h` home, `g n` notes, `g m` meetings, `g a` history, `g s` statistics, `g ,`
settings, `j` and `k` next and previous item, `Enter` open, `x` done or not done, `c` copy,
`e` export (the item under the cursor, the selection, or the view), `s` select, `Esc` clear,
`?` the list of keys.
