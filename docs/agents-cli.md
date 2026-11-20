# Agents on the command line

The commands that apps (Zone for Mac, the page, scripts) use to show and change how AI agents use
the library: the master switch, the example prompts, and what to paste into an app that zone
cannot register with by itself. This page gives the commands, their exit codes and the exact
JSON. Names and meanings here are stable. Fields may be added, none will be renamed or removed.

Every command exits 0 when it did what was asked and 1 with one line on standard error when it
could not (an unknown prompt or client, a library that cannot be opened).

## `zone agents on|off|status [--json]`

One switch for every agent on this computer. It is on until the person turns it off, so a
library from before the switch existed behaves exactly as it did.

```sh
zone agents off            # every agent stops reading the library
zone agents on             # agents may use it again, each within the view it was registered with
zone agents status --json
```

The setting lives in the library (the `meta` table, key `agents`, value `off`; absent means on),
so every `zone` program reads the same one. Switching to what it already is changes and records
nothing.

### `zone agents status --json` (also printed by `on` and `off` with `--json`)

```json
{
  "agents": "off",
  "enabled": false,
  "changed_at": 1795111699.342012,
  "changed_local": "2026-11-19T10:08:19-08:00",
  "message": "AI agents cannot read the library: every zone server answers that access is off. Turn it back on with `zone agents on` in a terminal, the AI agents switch in Zone for Mac's Settings, or the switch under Settings, AI agents on the page `zone open` shows."
}
```

| Field | Meaning |
| --- | --- |
| `agents` | `on` or `off`. |
| `enabled` | The same as a boolean. |
| `changed_at` | When it was last switched, Unix seconds. Null when it never was. |
| `changed_local` | The same moment as RFC 3339 in the computer's time zone, or null. |
| `message` | One sentence for a person, saying what the state means. |

`zone status --json` carries the same object as `agents`. `zone doctor` shows a note while it is off
(a note, not a failure: off is a choice).

### What an agent sees while it is off

Off does not unregister zone from any client. Each client still starts `zone mcp`, and the server
completes the handshake, so the client does not show a broken server. Then:

- `tools/list` offers one tool, `sync_status`, titled "Zone: agent access is off", with no
  arguments and `readOnlyHint: true`. Calling it succeeds with
  `{"agent_access": "off", "message": "<the sentence below>"}`.
- Calling any other tool, including one the client remembered from before, returns a tool result
  with `isError: true` and the sentence as its text.
- `prompts/list`, `resources/list` and `resources/templates/list` are empty. `prompts/get` and
  `resources/read` return a JSON-RPC error (`-32600`) whose message is the sentence.
- The `instructions` of a server started while it is off are the sentence.

The sentence:

> Agent access to Zone is turned off on this computer, so nothing from the person's library can
> be read through this connection. Tell the person. Turn it back on with `zone agents on` in a
> terminal, the AI agents switch in Zone for Mac's Settings, or the switch under Settings, AI
> agents on the page `zone open` shows.

A server reads the switch on every request, so the change applies to its next request. It also
looks every two seconds and, when the switch changed, sends `notifications/tools/list_changed`,
`notifications/prompts/list_changed` and `notifications/resources/list_changed`, so a client that
keeps its lists refreshes them without a restart.

### In the feed

Each switch adds one line to `zone events`:

```json
{"seq":12,"at":1795111699.3,"time":"2026-11-19T10:08:19-08:00","event":"agents_switched","on":false,"by":"cli"}
```

`on` is the new state. `by` is `cli` (`zone agents`), `page` (the page's Settings) or `mac` (Zone
for Mac, when it says so).

## `zone prompts [--json]` and `zone prompts show <id>`

Example prompts that play to what zone has: the person's spoken library. The same list is offered
to agents as MCP prompts (slash commands in clients that support them) and shown on the page under
Settings, Things to ask.

```sh
zone prompts                                   # id, title and one line each
zone prompts --json                            # everything below
zone prompts show what-did-i-say day="Wednesday 12pm to 5pm"
zone prompts show find-topic topic="the lease" --json
zone prompts --notes-only --read-only          # the prompts, and the wording, such an agent gets
```

The view flags of `zone mcp` (`--kinds`, `--exclude-sources`, `--notes-only`, `--no-keyboard`,
`--read-only`) work here too: a prompt that needs kinds the view hides is left out, the kinds a
prompt names are only the ones the view shows, and a prompt that marks items done says instead
that the connection is read-only.

The ids, in order: `notes-to-plan`, `work-through-notes`, `what-did-i-say`, `last-meeting`,
`just-sent`, `find-topic`, `weekly-review`, `draft-from-dictations`, `how-i-write`, `open-loops`.
More may be added.

### `zone prompts --json`

```json
{
  "prompts": [
    {
      "id": "what-did-i-say",
      "title": "What did I say on a day",
      "description": "A recap of everything you said in a day or a window of time, grouped by topic.",
      "kinds": ["dictation", "note", "meeting"],
      "writes": false,
      "arguments": [
        {
          "name": "day",
          "description": "The day or window, in words: today, yesterday afternoon, Wednesday, last Friday, 2026-11-11, Wednesday 12pm to 5pm.",
          "required": false,
          "default": "today"
        }
      ],
      "text": "What did I say today? Call list_items with when \"today\" and kinds [\"dictation\",\"note\",\"meeting\"], check that its range is the window I mean, ..."
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `id` | Stable. The MCP prompt's name, and what `zone prompts show` takes. |
| `title` | A few words, for a list. |
| `description` | One line, for under the title. |
| `kinds` | The kinds of item it works on. It is offered when the view shows at least one. |
| `writes` | True when it asks the agent to mark items done (`mark_processed`). |
| `arguments` | What can be filled in. A `required` one with no `default` shows as `[topic]` in `text`. |
| `text` | The prompt, ready to copy: defaults filled in, required arguments as `[name]` placeholders. |

`zone prompts show <id> --json` prints one such object, with `text` filled from the `name=value`
pairs given. Without `--json` it prints the text alone, ready to pipe or paste.

## `zone mcp config [--client <name>] [--json]`

What to paste into an app's MCP settings to use this zone, with this program's absolute path, for
apps zone cannot register with by itself ("Other app" with a Copy button).

```sh
zone mcp config                        # the generic JSON, then Codex's TOML, Hermes's and Goose's YAML
zone mcp config --client other         # the generic JSON only
zone mcp config --client codex         # one client's own snippet, and the file it goes in
zone mcp config --notes-only --json    # every form, for a server that shows only voice notes
```

The view flags go into the snippet's `args`. `--client` takes any name `zone mcp status` lists, or
`other` (also `generic`).

### `zone mcp config --json`

```json
{
  "name": "zone",
  "command": "/Users/you/.local/bin/zone",
  "args": ["mcp"],
  "env": {},
  "generic": {
    "client": null,
    "label": "Any other app",
    "format": "json",
    "path": null,
    "path_shown": null,
    "key_path": ["mcpServers", "zone"],
    "snippet": "{\n  \"mcpServers\": {\n    \"zone\": {\n      \"command\": \"/Users/you/.local/bin/zone\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n",
    "command_line": null
  },
  "clients": [
    {
      "client": "claude-code",
      "label": "Claude Code",
      "format": "json",
      "path": "/Users/you/.claude.json",
      "path_shown": "~/.claude.json",
      "key_path": ["mcpServers", "zone"],
      "snippet": "{\n  \"mcpServers\": {\n    \"zone\": {\n      \"type\": \"stdio\", ...",
      "command_line": "claude mcp add --scope user zone -- \"/Users/you/.local/bin/zone\" mcp"
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `command`, `args`, `env` | What an app should start. `env` holds `ZONE_HOME` only when this zone was run with it. |
| `generic` | The usual `{"mcpServers": {"zone": {...}}}` most apps take. |
| `clients` | One entry per client zone knows (one, with `--client`), in that client's own format. |
| `format` | `json`, `toml` or `yaml`. |
| `path` | The settings file the client reads (its first place, when it has several). Null for `generic`. |
| `path_shown` | The same with the home folder as `~`. |
| `key_path` | The keys the entry sits under. The last is always `zone`. |
| `snippet` | The text zone would write into an empty settings file. Merge it into an existing one, or run `zone mcp install <client>`, which does that without touching anything else. |
| `command_line` | A command that registers it instead, when the client has one (Claude Code). |

## `zone mcp status --json`: the `launch` of each place

Each entry of `clients` now also says what that place's config starts, exactly as written there,
whichever copy of zone (or other program) it is. Apps no longer need to read each client's file
to find out.

```json
{
  "client": "cursor",
  "label": "Cursor",
  "profile": null,
  "path": "/Users/you/.cursor/mcp.json",
  "installed": true,
  "state": "registered",
  "detail": null,
  "read_only": false,
  "scope": { "limited": false, "kinds": ["dictation", "note", "meeting", "clip"], "exclude_sources": [] },
  "launch": { "command": "/Users/you/.local/bin/zone", "args": ["mcp"], "env": {} }
}
```

`launch` is null when the place has no zone entry. `env` is an object of strings (`{}` when there
is none). For opencode, whose config keeps the program and its arguments in one list, `command` is
the first item and `args` the rest.
