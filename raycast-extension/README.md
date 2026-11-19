# Zone for Raycast

Your [Zone for iPhone](https://apps.apple.com/app/id6830955448) dictations, voice notes, meeting
transcripts and clips, searchable from Raycast, and readable by Raycast AI.

The extension reads the copy of your library that [`zone`](https://github.com/zaydiscold/zone-mcp)
keeps on your Mac. It runs the `zone` command line program and nothing else: no network requests,
no analytics, no account. Your words stay where zone keeps them; the extension caches none of
them on disk.

## Commands

| Command | What it does |
| --- | --- |
| Search Zone | Search everything you said and sent, by day. Filter by kind (Notes, Meetings, Dictations, Clips). The full text sits beside the list. |
| Open Notes | Your voice notes that are not done yet, newest or oldest first: your to-do list. Mark as Done (Cmd+D) ticks one off. |
| Paste Latest Clip | Pastes the newest text you sent from your iPhone into the app in front, or copies it (a preference of the command). |
| Recent Clips | Everything you sent from the phone, newest first. |
| Search Meetings | Meetings with their title, date, length and speakers; the transcript by speaker beside the list. |
| Zone Status | Whether zone is running, which phones are paired, the last sync and the counts, with Open Zone Page, Open Flash and Pair a Phone. |

Every item has Paste to Active App, Copy Text (Copy Transcript for a meeting), Copy as Markdown,
Open in Zone Page (zone's local page, at that item) and, for a note, Mark as Done or Mark as Not
Done.

Zone Status is a plain view and not a menu bar command, because Zone for Mac already has its own
menu bar app.

## AI tools

Mention `@zone` in Raycast AI, or use the tools from another agent through Raycast's MCP server.

| Tool | What it does |
| --- | --- |
| `search-zone` | Full-text search, newest first, with snippets and a cursor for more. |
| `list-zone-items` | Items in a time window (`since`, `until`), some kinds, newest or oldest first. |
| `get-zone-items` | The full text of items by id. A meeting comes back one line per speaker turn. |
| `get-latest-clip` | The newest clip: "the thing I just sent you". |
| `get-open-notes` | Voice notes not marked done, with their text. |
| `get-latest-meeting` | The most recent meeting with its transcript. |
| `mark-notes-done` | Marks notes done or not done. Raycast asks you to confirm first. The only tool that changes anything, and it changes only the done flag. |

Zone items can also be mentioned from the AI Chat composer's `@` menu (Zone Items).

The instructions and evals for Raycast AI are in [`ai.yaml`](ai.yaml).

## Requirements

- macOS and Raycast 2.13 or later.
- `zone` installed on the Mac:

  ```sh
  curl -fsSL https://raw.githubusercontent.com/zaydiscold/zone-mcp/main/install.sh | sh
  zone install   # start zone at login
  zone pair      # pair your iPhone
  ```

  or Zone for Mac, which brings `zone` with it.
- Zone on your iPhone, paired with the Mac (Settings > Computers), or a Zone backup folder.

The extension finds `zone` in `~/.local/bin`, `/usr/local/bin`, `/opt/homebrew/bin` or inside
`Zone.app`. Set the **zone Path** preference to use another copy, and **Library Folder** to read
another library (zone's `ZONE_HOME`).

Reading works whether or not zone is running. Opening zone's page, Flash and pairing need zone
running.

## Try it locally

```sh
cd raycast-extension
npm install && npm run dev
```

`npm run dev` builds the extension and adds it to Raycast in development mode. Stop it with
Ctrl-C. The extension then stays in Raycast until you remove it: Manage Extensions, select Zone,
then Remove Extension.

To try it on a made-up library instead of your own, fill a throwaway one and point the Library
Folder preference at it:

```sh
export ZONE_HOME=$(mktemp -d)
zone dev synthetic --demo --count 600 > demo.jsonl
zone serve --port 47899 &                       # a second zone, on its own port
zone pair --json --no-wait                      # copy the "url"
zone dev fake-phone --url '<url>' --identity phone.json --file demo.jsonl --addr 127.0.0.1:<zone's UDP port>
```

## Testing

```sh
npm run lint     # ray lint: manifest, ESLint and Prettier
npm run build    # ray build: type check and a distribution build
npm test         # the parts that do not run zone
ZONE_TEST_BIN=$(which zone) ZONE_TEST_HOME=$ZONE_HOME npm test   # and the zone calls, on a throwaway library
```

The tests check that paging covers every item once in both orders, that search pages with zone's
cursor, that unknown ids are reported, and that done is set and cleared. Without the two variables
only the parts that do not run zone are tested.

`npx ray evals` runs the AI evals in `ai.yaml` (it needs `npx ray login`).

## How it works

- Every call runs `zone` with an argument array, never through a shell, with
  `ZONE_NO_UPDATE_CHECK=1`.
- Lists page 50 items at a time. `zone search` has a cursor; `zone list` does not, so the next page
  of a list asks for items before (or after) the last item's time and skips the ones already shown.
- Typing in Search Zone waits 200 ms, and a new search stops the run of zone it replaces.
- A voice note is listed once: the recording behind it (a dictation with source `note`) is left
  out, as on zone's page.
- zone's command line has no command to mark an item done, so Mark as Done asks zone's own MCP
  server (`zone mcp`, its `mark_processed` tool) for it: the same flag an AI agent sets and the
  page's Done ticks. Nothing else in the library is changed.

## Publishing (not done)

Nothing has been published. To publish later:

1. Set `author` in `package.json` to the owner's Raycast Store handle (it must exist:
   `npm run lint` checks it), and run `npm run lint` and `npm run build`.
2. Add Store screenshots to a `metadata` folder: 2000 x 1250 PNG, taken with Raycast's Capture
   Window command on a made-up library (see above), up to six.
3. Either run `npm run publish`, which opens a pull request in
   [`raycast/extensions`](https://github.com/raycast/extensions), or fork that repository, copy this
   folder to `extensions/zone`, and open the pull request yourself. Raycast reviews it and
   publishes it to the Store when it is merged.
