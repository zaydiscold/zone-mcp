# Zone Link protocol, version 1

How an iPhone running Zone gets its library (dictations, notes, meeting transcripts) onto a
computer running `zone`, the program in this repository. The phone is the source of truth and
always the sender. The computer keeps a copy in SQLite and serves it to people and agents.

Three carriers move the same items. All of them end in the same upsert into the same database.

| Carrier | Network needed | Phone change needed | Automatic |
| --- | --- | --- | --- |
| Link (iroh) | any internet, or the same LAN | yes | yes |
| Flash (animated QR, phone screen to computer camera) | none | yes | no, you hold the phone up |
| Backup folder (a Zone backup in iCloud Drive, OneDrive, Dropbox) | whatever the folder sync uses | no | when Zone writes the backup |

## Items

One item is one JSON object. Unknown fields are ignored by receivers and never an error.

```json
{
  "id": "9F1C2E6A-0000-4000-8000-000000000001",
  "kind": "dictation",
  "source": "keyboard",
  "created_at": 1794445200.25,
  "updated_at": 1794445203.5,
  "text": "the words",
  "title": null,
  "duration_s": 4.2,
  "engine": "fluidMini",
  "status": "finished",
  "extra": {},
  "rev": "5b1f0c9a2d3e4f60"
}
```

- `id`: the phone's stable UUID for the record, uppercase with hyphens. Primary key together with nothing else: ids are unique across kinds.
- `kind`: `dictation`, `note` or `meeting`. Required. This is the label agents filter on.
  - `dictation`: something spoken into a text field or the app. Mostly messages and text typed by voice in other apps.
  - `note`: a voice note saved in Zone's own library.
  - `meeting`: a recorded meeting with a transcript.
- `source`: for dictations, where it was spoken: `keyboard`, `app`, `note`, or any other raw value the phone uses (pass through unchanged). Null for notes and meetings.
- `created_at`, `updated_at`: Unix seconds (since 1970-01-01 UTC), fractional. `updated_at` may equal `created_at`.
- `text`: the full text. For a meeting, the whole transcript as readable text, one line per segment, `Speaker name: words` when speakers are known.
- `title`: meetings only, else null.
- `duration_s`: recorded audio seconds when known, else null.
- `engine`, `status`: passed through from the phone, may be null.
- `extra`: kind-specific details. For a meeting: `{"segments":[{"speaker":"Name or null","start":12.5,"end":15.0,"text":"..."}]}`. For a dictation, optional `recovered_from` (Unix seconds) and `destination`.
- `rev`: an opaque version string chosen by the sender (the phone uses a hash of the record's content). The computer stores it and never interprets it.

A deletion is `{"id":"...","kind":"dictation","deleted":true,"updated_at":1794445300.0}`. The computer marks the item deleted (keeps the row, drops the text). Items the phone merely aged out of its own history (retention) are not deletions and are never sent as such: the computer is the long-term copy.

Merge rule on the computer:

- From the phone itself (Link and Flash), the phone is the source of truth: an incoming item always replaces the stored row, and an incoming live item brings back a row that was deleted (the person restored it on the phone).
- From a backup folder (which can be stale): insert when the id is new; otherwise replace only when the incoming `updated_at` is greater than or equal to the stored one, and never bring back a deleted row.

## Carrier 1: Link (iroh)

Both sides are iroh endpoints (iroh 1.x). The computer runs `zone serve` and accepts; the phone dials the computer's endpoint id. iroh gives an authenticated, encrypted QUIC connection between the two keys, direct when it can (same LAN, hole punching) and through an iroh relay when it cannot. The relay forwards encrypted packets and stores nothing.

- ALPN: `zone-sync/1` (ASCII bytes).
- The phone opens one bidirectional stream per session.
- Framing in both directions: a 4-byte big-endian unsigned length, then that many bytes of UTF-8 JSON (one object). Maximum frame 4 MiB (4194304). A larger length is a protocol error: close.
- Either side closes a session with no frame for 60 seconds.

### Pairing link

`zone pair` shows a QR code of this URL, and prints the URL too:

```
zone://pair?v=1&n=<endpoint id>&s=<secret>&name=<computer name>&r=<relay url>
```

- `n`: the computer's iroh endpoint id (public key), in iroh's standard text form.
- `s`: 32 random bytes as 64 lowercase hex characters. One use, expires 10 minutes after it is shown.
- `name`: percent-encoded display name of the computer.
- `r`: optional, percent-encoded relay URL when the computer is pinned to one. Usually absent: iroh's default discovery finds the computer from `n`.

### Messages

Every object has `t`. Phone speaks first.

1. Phone: `{"t":"hello","v":1,"phone":{"id":"<install UUID>","name":"Zayd's iPhone","app":"1.0.3 (2026111401)"},"pair":"<secret hex>"}`
   - `pair` is present only on the first connection after scanning a pairing link.
2. Computer, one of:
   - `{"t":"welcome","v":1,"computer":{"id":"<database UUID>","name":"frostbyte","os":"macos","version":"0.1.0"},"items":1629,"resend_all":false}`
   - `{"t":"refused","reason":"not_paired"}` then close. Reasons: `not_paired`, `bad_secret`, `version`, `busy`.
3. Phone, zero or more: `{"t":"items","seq":1,"items":[ ... ]}`. At most 500 items and 1 MiB of JSON per frame; `seq` starts at 1 and counts up within the session.
4. Computer, for each: `{"t":"ack","seq":1,"stored":500}` after the batch is committed to disk.
5. Phone: `{"t":"bye"}`. The computer finishes its stream and the session ends.

Rules:

- The computer authorizes a session by the phone's iroh endpoint id (authenticated by the connection). With a valid unexpired unused `pair` secret it pins that endpoint id as a paired phone, atomically consuming the secret. Without `pair`, the endpoint id must already be pinned, else `refused not_paired`. A wrong or expired secret is `refused bad_secret`.
- `computer.id` is the UUID of the computer's database, created with the database. The phone remembers, per computer, which item `rev`s that database has acknowledged. If `computer.id` differs from the one it remembered, or `resend_all` is true, the phone forgets what it sent and sends everything.
- The phone treats an item as delivered only after the `ack` for its frame.
- Sending the same item twice is harmless.

### When the phone syncs

On pairing (everything), a few seconds after any dictation, note or meeting is saved or changed (debounced), whenever Zone comes to the foreground, and on a manual "Sync now". If the computer is unreachable the items simply stay pending for the next attempt.

## Carrier 2: Flash

For networks that block everything, and for the fun of it. The phone shows a rapid sequence of QR codes; the computer's camera reads them through the local page `zone open` serves (the browser's camera API works on `http://127.0.0.1`). No network is involved at any point.

- Payload: raw DEFLATE (RFC 1951, no zlib or gzip header; this is what Apple's `COMPRESSION_ZLIB` and `NSData.compressed(using: .zlib)` produce) of the UTF-8 JSON `{"t":"flash","v":1,"phone":{...same as hello...},"items":[...]}`, then sealed with ChaCha20-Poly1305 using the 32-byte flash key, as `nonce(12) || ciphertext || tag(16)`.
- Flash key: `HKDF-SHA256(ikm = pairing secret bytes, salt = empty, info = "zone-flash/1")`, 32 bytes, derived by both sides at pairing and kept. A flash therefore only works with a computer the phone has paired with (over Link, or by scanning the pairing QR while offline: scanning alone is enough to learn the secret, and the computer accepts a first flash sealed with a still-valid pairing secret as that phone's pairing).
- Chunking: the sealed payload is cut into `k` chunks of `c` bytes (default `c` = 300, the last chunk zero padded; `k` at most 65535). Frame `i` (0, 1, 2, ... without end) is a QR code in byte mode, error correction level L, whose bytes are:
  `"ZF1"` (3 ASCII bytes) `|| session(u32 BE, random, same for the whole flash) || total_len(u32 BE) || k(u16 BE) || i(u32 BE) || body(c bytes)`. The header is 17 bytes, so `c` is the frame length minus 17.
- Fountain code: the body of frame `i` is the XOR of a set of chunks, so the receiver never needs one particular frame, only enough different ones.
  - For `i < k` the set is just chunk `i` (the plain data goes first).
  - For `i >= k`: take `ceil(k/64)` 64-bit words from splitmix64 started at `state = (session << 32) | i`. Each word is: `state += 0x9E3779B97F4A7C15; z = state; z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9; z = (z ^ (z >> 27)) * 0x94D049BB133111EB; z ^= z >> 31` (all modulo 2^64). Chunk `n` is in the set when bit `n mod 64` of word `n / 64` is 1. If the set comes out empty, it is chunk `i mod k`.
  - The receiver keeps each received frame as a row (which chunks, the body) and solves by elimination over GF(2). It is done when it has `k` independent rows, usually after about `k + 10` frames, in any order, with any frames missing.
- Frame rate: 12 frames per second by default. The page shows progress; the phone keeps going until the person taps Done (or it has shown `3k + 60` frames, then starts again from `i = 0`).
- Sizes: `c` = 300 gives a version 11 QR code, which an ordinary laptop camera reads at arm's length. At 12 frames a second that is about 3.6 kB of compressed data a second: a day of dictation in a few seconds, a whole library of a thousand items in a minute or two.

`scripts/flash_reference.py` is the reference encoder and decoder. Both implementations must reproduce its vector: session `0x01020304`, payload byte `i` = `(i*7+3) mod 256` for 1280 bytes, `c` = 300, so `k` = 5. The chunk sets of frames 5 to 12 are `[1] [0,3] [1] [0,3,4] [1,2,4] [1] [1,2,3] [2,4]`, and the SHA-256 of frames 0 to 39 concatenated is `16e659e1a7b1a24735a6f04aeca9514d3ab1a59f285993c2b1e6e97a00c39b12`.

The computer accepts a flash from a paired phone, or from a phone that has scanned a still-valid pairing code (the payload opens with that offer's flash key, which pairs the phone by its `phone.id`). Flash items are merged like Link items.

## Carrier 3: Backup folder

Zone can already keep a backup in any Files location (iCloud Drive, OneDrive, Dropbox). The package is a folder named `<something>.zonebackup` containing `library.json`:

```json
{ "formatVersion": 1, "createdAt": <seconds since 2001-01-01 UTC>,
  "notes": [ {"id","text","createdAt","editedAt"?} ],
  "dictations": [ {"id","text","createdAt","updatedAt","audioSeconds","engine","source","status", ...} ],
  "meetings": [ {"id","title","createdAt","updatedAt","status","engine","segments","speakers", ...} ] }
```

All dates in `library.json` are seconds since 2001-01-01 00:00:00 UTC (add 978307200 for Unix seconds). `zone import <path>` reads it once; `zone backup-folder <path>` makes the running daemon re-import whenever `library.json` changes. Imported items use the same ids as the phone, so the backup and the Link merge into one set of rows.
