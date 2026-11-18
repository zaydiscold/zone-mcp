# Zone Link protocol, version 1

How an iPhone running Zone gets its library (dictations, notes, meeting transcripts) onto a
computer running `zone`, the program in this repository. The phone is the source of truth and
always the sender. The computer keeps a copy in SQLite and serves it to people and agents.

Three carriers move the same items. All of them end in the same upsert into the same database.

| Carrier | Network needed | Phone change needed | Automatic |
| --- | --- | --- | --- |
| Link (iroh) | any internet, or the same LAN | yes | yes |
| Flash (animated QR, black and white or color, phone screen to computer camera) | none | yes | no, you hold the phone up |
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
- `kind`: `dictation`, `note`, `meeting` or `clip`. Required. This is the label agents filter on.
  - `dictation`: something spoken into a text field or the app. Mostly messages and text typed by voice in other apps.
  - `note`: a voice note saved in Zone's own library.
  - `meeting`: a recorded meeting with a transcript.
  - `clip`: text the person sent to the computer on purpose (see Clips below). Not something they said.
- `source`: for dictations, where it was spoken: `keyboard`, `app`, `note`, or any other raw value the phone uses (pass through unchanged). Null for notes and meetings.
- `created_at`, `updated_at`: Unix seconds (since 1970-01-01 UTC), fractional. `updated_at` may equal `created_at`. A sender writes each with enough digits to read back as the same binary64 number (the shortest that does, as Swift and Rust print a `Double`), and a receiver parses numbers correctly rounded, so a time arrives to the last bit. (zone 0.1.0 parsed about one time in five one binary digit off, a few tenths of a microsecond.)
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

One thing on the computer comes before that rule. The person can remove an item from the computer on the local page (the phone is not told, and agents cannot do it). The computer remembers the item's `updated_at` at that moment, and from then on, for every carrier:

- the same item again, or an older version of it, changes nothing: it stays removed (and is acknowledged like any other item, so the phone does not keep sending it);
- a version whose `updated_at` is greater than the remembered one takes its place and the item is back: the person edited it on the phone;
- a deletion from the phone (or from a backup at least as new) makes it an ordinary deleted item.

A removed item is kept as a deleted row without its text, so every reader leaves it out, and what it held is kept beside it for 30 days so the person can put it back. After that the text is erased and only the id and the remembered `updated_at` stay.

## Clips

A clip is a piece of text the person chose to send from the phone to the computer: what was on the phone's clipboard, something shared from another app, or something typed. It is an ordinary item with `kind` `clip`, so every carrier moves it and every reader can list it.

- `source`: how it was sent: `clipboard`, `share`, `shortcut` or `typed`.
- `text`: the text, at most 256 kB of UTF-8. `title`, `duration_s`, `engine` and `status` are null.
- `created_at`: when the person sent it.
- The phone sends a clip at once (no waiting, unlike other changes) and holds it until the computer acknowledges it. A clip goes to every paired computer whose sending is switched on.
- Clips are not part of the phone's library and are not in a backup folder. Deleting one on the phone is not sent as a deletion.

On the computer:

- A clip is stored like any item. `zone clip` prints the newest one, `zone clip --copy` also puts it on the computer's clipboard.
- With the clipboard setting on (`zone clipboard on`, off until the person turns it on), a clip that arrives over Link and whose `created_at` is less than 120 seconds old is put on the computer's clipboard as it arrives. Older clips (a resend, a Flash, a phone that was offline) are stored and never touch the clipboard.
- The activity feed records that a clip arrived and its length, never its text.

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

1. Phone: `{"t":"hello","v":1,"phone":{"id":"<install UUID>","name":"My iPhone","app":"1.0.3 (2026111401)"},"pair":"<secret hex>"}`
   - `pair` is present only on the first connection after scanning a pairing link.
   - `phone.appearance` and `phone.accent` are optional (newer Zone builds for iPhone may send them; a receiver must not require them, and zone 0.1.2 and earlier ignore them). They say how Zone looks on the phone, so the computer's page can match it:
     - `appearance`: `"dark"` or `"light"`, the appearance Zone is drawn in on the phone at the time of the hello. When the app's own setting is Match iPhone, send what that comes to now (the system's current appearance), never `"system"`.
     - `accent`: the raw value of the accent chosen in Settings, Appearance: `"violet"`, `"ice"`, `"ember"`, `"mint"` or `"rose"`.
     - The computer stores each per phone when present and keeps the last value when a later hello leaves it out. A value it does not know is ignored. A flash's `phone` object may carry the same two fields.
     - The phone should start a session (an empty one is fine: hello, welcome, bye) when either changes, so the computer learns of it.
2. Computer, one of:
   - `{"t":"welcome","v":1,"computer":{"id":"<database UUID>","name":"studio","os":"macos","version":"0.1.1"},"items":1629,"resend_all":false}`
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

- Payload: raw DEFLATE (RFC 1951, no zlib or gzip header; this is what Apple's `COMPRESSION_ZLIB` and `NSData.compressed(using: .zlib)` produce) of the UTF-8 JSON `{"t":"flash","v":1,"phone":{...same as hello...},"check":{...},"items":[...]}`, then sealed with ChaCha20-Poly1305 using the 32-byte flash key, as `nonce(12) || ciphertext || tag(16)`.
- `check`, optional (newer Zone builds for iPhone send it, earlier ones do not, so receivers must not require it; zone 0.1.0 ignores it): the phone's own summary of the items, `{"items":1764,"oldest":1741000000.25,"newest":1794800000.5,"digest":"<64 hex>"}`.
  - `items`: how many entries `items` has.
  - `oldest`, `newest`: the smallest and largest `created_at` among the entries that are not deletions. Left out when there are none.
  - `digest`: lowercase hex SHA-256 over six lines per entry, in order, each line ended by a line feed (0x0A): the `id`, the `kind` (each `-` when absent or not a string), `1` when `deleted` is true and `0` otherwise, `created_at` and `updated_at` each as the 16 lowercase hex digits of the number's IEEE 754 binary64 bits with minus zero read as zero (`-` when absent or null), and the lowercase hex SHA-256 of `text` as UTF-8 (`-` when absent or null). A deletion carries neither `created_at` nor `text`, so both are `-` for it. The times go in as bits, so a time that changed in its last binary digit on the way does not match. `scripts/flash_reference.py` computes it; its vector (`CHECK_VECTOR`, three items including a time that a lossy parser reads one digit off and a text with an emoji, Hebrew and a NUL) has the digest `9bbd47a61c92a30de9cbd968fa4de4bed8e6d230d17ed7dabbfa6a63f3d02a6f`.
  - The receiver compares all four with what it parsed and says which did not match. After storing it reads every item back from its library (the last entry of each id, which is the one kept) and compares it field for field with what arrived. The person is told "1,764 items, all checked" or what did not check out. A mismatch is reported, not refused: the seal already proves the bytes are the phone's, so a mismatch means the two sides read them differently, which is a bug to see, not data to drop.
- Flash key: `HKDF-SHA256(ikm = pairing secret bytes, salt = empty, info = "zone-flash/1")`, 32 bytes, derived by both sides at pairing and kept. A flash therefore only works with a computer the phone has paired with (over Link, or by scanning the pairing QR while offline: scanning alone is enough to learn the secret, and the computer accepts a first flash sealed with a still-valid pairing secret as that phone's pairing).
- Chunking: the sealed payload is cut into `k` chunks of `c` bytes (the last chunk zero padded; `k` at most 65535). The sender chooses `c`, up to 2936 (the largest QR code holds 2953 bytes), and keeps it for the whole flash; the receiver learns it from the length of any frame. Frame `i` (0, 1, 2, ... without end) is a QR code in byte mode, error correction level L, whose bytes are:
  `"ZF1"` (3 ASCII bytes) `|| session(u32 BE, random, same for the whole flash) || total_len(u32 BE) || k(u16 BE) || i(u32 BE) || body(c bytes)`. The header is 17 bytes, so `c` is the frame length minus 17.
- Fountain code: the body of frame `i` is the XOR of a set of chunks, so the receiver never needs one particular frame, only enough different ones.
  - For `i < k` the set is just chunk `i` (the plain data goes first).
  - For `i >= k`: take `ceil(k/64)` 64-bit words from splitmix64 started at `state = (session << 32) | i`. Each word is: `state += 0x9E3779B97F4A7C15; z = state; z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9; z = (z ^ (z >> 27)) * 0x94D049BB133111EB; z ^= z >> 31` (all modulo 2^64). Chunk `n` is in the set when bit `n mod 64` of word `n / 64` is 1. If the set comes out empty, it is chunk `i mod k`.
  - The receiver keeps each received frame as a row (which chunks, the body) and solves by elimination over GF(2). It is done when it has `k` independent rows, usually after about `k + 10` frames, in any order, with any frames missing.
- Every frame says how many pieces the flash has (`k`) and which frame it is (`i`). The page counts code numbers from 1 (frame `i` is code number `i + 1`): how many different ones were caught, the highest seen, and which below it were missed ("of codes 1 to 512, the camera missed number 70 and numbers 141 to 143"), with how many pieces are still needed. Missing codes is harmless: any code stands in for any other. Only code numbers the phone can have shown are counted: those in its loop of `3k + 60`, and past that (a sender that does not loop) only ones within 256 of another caught number. A code whose number was misread into some other number is left out, where zone 0.1.0 let a single one make the Done page say "Of codes 1 to 1,509,950,965".
- A flash goes by a name: its session as eight lowercase hexadecimal digits in two groups of four, `8f3a 21c4`. The iPhone shows it under its codes and in its Done question ("Done for flash 8f3a 21c4, with 153 items"), and the page shows it while reading and when done, so the person can tell the two are talking about the same flash.

#### Misread codes

QR error correction rejects nearly all damage, but a code can still be read as another valid code with the wrong bytes: caught half drawn, or a color channel unmixed wrongly. ZF1 frames carry no checksum of their own, and one wrong frame among the rows makes the whole payload wrong. The receiver finds it from the frames themselves (zone: `src/flash/assemble.rs`):

- A frame that brings no new row is a check: its chunk set is a sum of the held rows', so its body must be the same sum of their bodies. When every frame is what the phone showed, the difference (the residual) is zero. The same frame number read twice with different bodies fails this check too.
- Each row remembers which frames it was made of, so an uneven check names a set of frames that contains a misread one. An even check clears the frames it names: the errors of different misreadings do not cancel. Uneven checks with the same residual add up the same errors, most often one misread frame's, so each group narrows to its own culprit. A frame left as the only suspect of a group is set aside for good (its number and body) and taken out of the rows: of the rows made of it, the one with the lowest pivot is added to each of the others (their pivots stay) and dropped, one pass over the rows. The uneven checks that named it are checked again against the rows as they are now (the newest 64), one of them often bringing back the row that left, and reading goes on. With patience, a few suspects that stop narrowing are set aside together; a good frame set aside by mistake costs only time.
- The phone shows each code once a loop, and a misreading does not come out the same twice. So a frame read again, body for body, in another showing is taken to be right, as if an even check had vouched for it: a check read again is no longer a suspect, and neither is a row whose frame was read again. Another showing means at least 16 other codes were shown in between, counted as moves: readings of a code number that is not among the last eight different ones read. A picture that stays up (a phone that stalls, or the color schedule's first and last pictures, up for about half a second each) is read again and again, misreading and all; those readings are one showing however many there are. Counting every reading instead, 17 readings of one still code (7 pictures of a still color one) would vouch for a misread row, after which the right reading of that code would look misread and be set aside, and so would good checks after it.
- Every step is bounded. One code looks for misread frames at most eight times and sets aside at least one each time. After 8 payloads that did not open, 32 takings out, or too many uneven checks since reading began (256, plus four for every piece up to 2048 more), misread rows are so many that no check names just one of them, and the search turns thorough: after each setting aside it makes the rows again from every frame kept, which reshuffles what the checks say until they narrow. After 32 of those, or more uneven checks than that at once, the flash starts over from new frames, keeping the frames it set aside as the only suspect left. A frame set aside on weaker grounds (an uneven check whose rows had all been vouched for, or one of a few suspects set aside for lack of patience) may have been right, and is forgotten, so it counts again when it comes round. Making the rows again from every frame kept, the one long step, asks whether to stop every 16 frames, so Start over stops it at once; it is done again at the next code if the flash goes on. zone 0.1.0 made the rows again from every frame each time, in the request that brought the deciding code, and the search could start again from inside that: one step took 42 seconds at 16384 pieces with four misread codes.
- A frame whose header differs from a flash already being read by two bits or fewer (session, payload length and `k` together) is a misreading of that flash, not a new one. Two flashes made apart have random sessions about 16 bits apart.
- The seal is the final judge. A complete payload that no key opens is either a misread code or a flash sealed for another computer. The receiver keeps reading: if 16 later checks in a row come out even, every frame agrees and the payload is what the phone showed, and only then does it say the flash is from a phone this computer does not know. Until then the page says a code may have been misread and asks to keep holding the phone up.
- Every flash ends: it is stored, or the page says why not. A whole loop of the phone's codes (`3k + 60` moves) in which no new frame made a row or checked out even, and the rows did not become complete, means the codes bring nothing new: nearly all are known (as rows, checks, or set aside), and what is known does not open or does not come together. The receiver then helps the flash along, a step further each time this happens (a flash too large to track starts over at the first step): first it forgets which frames vouched for themselves by being read again, takes back the frames set aside on weaker grounds, and makes the rows again from every frame kept, which checks every check again against the rows as they are now; the second time it starts over from new frames, keeping only the frames set aside as the only suspect left; the third time it starts over keeping nothing; the fourth time it gives up. It also gives up, whatever it is doing, after 12 loops' worth of moves. Giving up fails the flash for good (until the page starts over), and the page says the codes never came together, that nothing was stored, what may cause it (glare, a dim screen, a phone too far away or moving, or a flash for another computer) and to start over with the phone steady and close.
- Remembering what each row was made of costs up to about `k * k / 16` bytes, so zone does it for flashes of up to 16384 pieces. A larger flash that goes wrong starts over from new frames: slower, never wrong.
- Measured on simulated damage (`zone`'s `stress_misread_rates`, 248 pieces, 30% of codes lost, twelve runs each, the camera watching up to 18 loops): the right library every time at 0.5, 2, 4 and 8 percent of codes misread, after 1.0 to 4.5 times `k` codes up to 4 percent, and at 8 percent after 4.6 to 7.1 times `k` in eleven runs and 23.3 times `k` in the twelfth, which stopped moving forward twice and was helped along (zone 0.1.0, the camera watching 8 loops: every time after 1.0 to 3.6 times `k` up to 4 percent, and seven runs of twelve at 8 percent). At 12 percent eleven runs stored the right library, after 5.7 to 34.5 times `k`, and one gave up after 12 loops, saying so; at 16 percent four stored, after 7.1 to 31.2 times `k`, and eight gave up. Nothing wrong was ever stored, and every run ended. Real misreads are far rarer.

#### Stored once

zone records every flash it stores in its library, in the same transaction as the items: its header (session, payload length, `k` and `c`), the digest of its items as they arrived (computed as for `check.digest`, whether or not the phone sent one), the phone, how many items, and when. Frames of a recorded flash, still on a phone's screen after the page started over or after zone restarted, are known from their header, and the page says the flash was stored before, and when; it is not stored again. Storing it again would put the phone's older version back over anything Link has updated since, because the phone wins merges. The header decides, not the digest: a new flash with the same items as an earlier one is stored as usual, and finds everything already there. A flash that failed for good is remembered only until the page starts over, so the same codes can be tried again after zone is updated. Neither kind takes the page over from a flash being read. (zone 0.1.0 remembered stored flashes in memory only, so after Start over or a restart the same flash was stored a second time.)

Storing a flash (opening it, pairing the phone when it was sealed with a pairing offer's key, merging every batch of items, recording the delivery and the flash) is one transaction. A library that fails part way keeps nothing of it, pairing included, so the next try pairs the phone, says so, and counts every item as new, exactly as the first try would have.

#### Size and time

- `k` may be up to 65535 and a receiver accepts that, but the work grows with the square of `k`. The iPhone keeps one flash to 4,096 pieces (3.7 MB sealed at `c` = 900): a larger library goes as several flashes, and Everything continues where the last confirmed flash stopped. A sender should keep `k` about as small.
- zone puts a flash together on a thread of its own. The page's requests hand it the codes from a picture and are answered as soon as they are taken in, or after a second at most with where the flash stood; while zone puts the pieces together or stores the items, its status says `storing`, and what it is doing (`work`: `solving` or `storing`), and the page says zone is putting the library together, not that the phone went out of sight. Start over stops a flash being put together, and a search for a misread code that is making the rows again.
- Times on one core of an Apple M4 Pro (release build), `c` = 900, 30% of codes lost, misread codes (one byte changed) among the first `k` codes read: `cargo test --release flash_assembly_times -- --ignored --nocapture`. "All the work" is every code reduced, every misread code found and taken out, and every solve; it is spread over the minutes the phone takes to show the codes (4,096 pieces at 20 codes a second take three and a half minutes). "Longest step" is the most any one code cost, which is felt at once: the final solve, or a misread code being taken out. The same codes, through zone 0.1.0, for comparison:

  | Pieces | Misread codes | Codes read | All the work | Longest step | zone 0.1.0: all the work, longest step |
  | --- | --- | --- | --- | --- | --- |
  | 1,024 | 0 | 1,025 | 10 ms | 1.8 ms | 151 ms, 36 ms |
  | 1,024 | 4 | 1,081 | 17 ms | 4.1 ms | 395 ms, 204 ms |
  | 1,024 | 8 | 1,540 | 60 ms | 4.8 ms | 1.0 s, 556 ms |
  | 4,096 | 0 | 4,098 | 155 ms | 18 ms | 2.6 s, 620 ms |
  | 4,096 | 4 | 4,161 | 198 ms | 28 ms | 4.9 s, 2.1 s |
  | 4,096 | 8 | 4,493 | 292 ms | 34 ms | 6.4 s, 2.9 s |
  | 16,384 | 0 | 16,386 | 4.9 s | 244 ms | 46.5 s, 10.1 s |
  | 16,384 | 4 | 16,442 | 5.3 s | 351 ms | 88.7 s, 42.0 s |
  | 16,384 | 8 | 17,019 | 6.9 s | 408 ms | 104.5 s, 50.4 s |

  Past 16384 pieces zone does not track what rows were made of, so a misread code costs a fresh start. At 65,535 pieces a clean flash took 119 s of work with a 9.1 s final solve; with one misread code it took 1,043 s and a 39 s solve, because after the fresh start every row was a coded frame. The most memory used was 0.49 GB. All of it runs on zone's flash thread, and the page is answered throughout.
- Frame rate: the sender's choice too. The receiver reads every camera picture it can, in any order, so a faster sender is simply faster as long as the camera still sees each code whole: a laptop camera takes 30 pictures a second, so up to about 20 codes a second each stay on screen for at least one full picture. Faster than that, codes are increasingly caught half drawn and missed, which the fountain code tolerates but which costs time. The page shows progress; the phone keeps going until the person taps Done (or it has shown `3k + 60` frames, then starts again from `i = 0`).
- Sizes: the larger `c`, the more each code carries and the denser it is. These are read times for the computer's reader (`zone dev flash-bench`, on one core of an Apple M4 Pro, release build), on pictures made to look like a webcam's: the code 520 pixels wide in the 720 pixel square, turned 4 degrees, slightly out of focus (blur radius 1) and noisy (plus or minus 6). Nothing here was filmed with a real camera.

  | `c` | Frame | QR code | Read time, median | Codes read | Data at 12, 20, 30 codes a second |
  | --- | --- | --- | --- | --- | --- |
  | 300 | 317 bytes | version 11, 61 modules | 2.0 ms | 100% | 3.6, 6.0, 9.0 kB/s |
  | 700 | 717 bytes | version 18, 89 modules | 2.3 ms | 100% | 8.4, 14, 21 kB/s |
  | 900 | 917 bytes | version 21, 101 modules | 2.5 ms | 100% | 10.8, 18, 27 kB/s |

  With the code only 450 pixels wide (about 4 pixels per module at `c` = 900), all are still read, and so are all of the version 18 codes in a harsh case (turned 10 degrees, blur radius 2, noise plus or minus 10). A picture with no code in it costs under 2 ms while the page waits for a phone, and about 12 ms while codes are in view, when a second reader takes a look too. Sent at the full 1080 pixel square of a 1080p camera, a picture takes about 4 ms. A color picture (below) carries three codes and takes 16 to 21 ms. The data rate counts compressed, sealed bytes: ordinary speech compresses to well under half its size.

### Color Flash

A picture may carry up to three frames at once, one in each color channel. Each is an ordinary frame as above (same `ZF1` format, same session); nothing in a frame says it travelled in color.

- In a channel a module is at one of two levels, a dark one and a light one. A picture holding frames A, B and C is red from A, green from B and blue from C, so every module is one of eight colors: all three dark, all three light, and the six in between.
- The frames of one picture have the same length, so their QR codes are the same version and their finder patterns, timing patterns, alignment patterns and quiet zone line up: the all-dark color where all three are dark, the all-light color where all three are light. A picture with fewer than three frames repeats one of them in the spare channels.
- Which frame goes in which channel, and which frames share a picture, is the sender's choice. The receiver must not depend on either: it reads each channel as a code of its own and keeps frames by `(session, i)` as always, so a frame seen twice counts once.
- Senders may still send black and white pictures, and may mix the two in one flash. The receiver reads a picture in gray when it is not a color code.
- A color picture carries three frames, so it moves three times the data of a black and white one at the same QR version and the same pictures a second.

#### The palette

The plain palette is dark 0 and light 255 in every channel: black, white, red, green, blue, cyan, magenta, yellow. The sender may instead give each channel its own two levels, so that the picture has a designed look. Written `r0,r1,g0,g1,b0,b1` (the dark then the light level of red, of green, of blue, each 0 to 255 in the screen's own color space, sRGB), the plain palette is `0,255,0,255,0,255`, and `60,255,0,200,90,255` is a palette whose all-dark color is a deep violet and whose all-light color is a pale magenta.

The contract, for the sender:

- **Constant within a picture.** Every module of a channel is exactly at that channel's dark level or its light level: no gradients, no third level, no antialiasing between modules. The quiet zone (four modules) is the all-light color `(r1, g1, b1)` and the dark of the finder patterns is the all-dark color `(r0, g0, b0)`.
- **Free between pictures.** Each picture may have its own palette. Nothing in a frame or a picture says which: the receiver works the palette out from each picture by itself, so there is nothing to keep in step.
- **Light above dark, by at least 120.** In every channel `light - dark` (the span) is at least 120. 150 or more is recommended until this has been filmed with real cameras. Where the span sits between 0 and 255 does not matter (`0,120`, `67,187` and `135,255` read alike).
- **Green carries the most.** A camera's green sees part of the red and part of the blue light, so green is the channel that suffers when it has less contrast than its neighbours. Give green a span at least as large as red's and blue's. Red or blue may have less than the others.
- **Three different frames, or one.** A picture needs all eight colors for the receiver to work out the palette (random-looking sealed data gives them). A picture that repeats a frame in two channels has only four and is read by its raw channels: use the plain palette for those, or black and white.
- Around the code, anything that is not busy: a plain background, the phone's own interface, the retro sweep between pictures. The receiver looks only at the middle of the code to learn the palette.

What the receiver has to undo: a camera sees each of the phone's colors partly in its neighbouring channels (crosstalk, commonly 10 to 30%), webcams store color at half the resolution of brightness (4:2:0), and exposure moves every level. What the camera shows of a module is close to `dark + M * s`, where `s` holds the module's three bits, `dark` is the all-dark color as the camera sees it, and the 3 by 3 matrix `M` holds both the palette's spans and the camera's mixing. The computer finds `dark` and `M` from each picture (`src/flash/color.rs`): it finds the code as the busy part of the picture and looks at the middle of it, finds the eight colors there (as the places the pixels are densest, or by sorting the pixels into eight groups, whichever explains the pixels better), tells which color is which from the shape the eight make (the corners of a slanted box, the all-dark color its darkest corner), and turns `M` around. Each channel then comes back on its own, stretched to full contrast whatever the span was, and is read like any black and white code.

Measured with `zone dev flash-sweep` (300 codes in each cell, three to a picture; version 21 codes, `c` = 900; the picture 1.4 times the code's width; one core of an Apple M4 Pro). The cameras are made up, and nothing here has been filmed with a real phone and camera yet:

| Camera | Turned | Blur radius | Noise | Crosstalk | Darker toward the far corner |
| --- | --- | --- | --- | --- | --- |
| normal | 3 degrees | 1 | plus or minus 6 | 0.15 | 10% |
| harsh | 6 degrees | 1 | plus or minus 10 | 0.3 | 25% |
| blurred | 6 degrees | 2 | plus or minus 10 | 0.3 | 25% |

Codes read out of 300, by span (the same in every channel, light at 255) and by the code's width in the picture:

| Span | Levels | normal 450, 550, 700 px | harsh 450, 550, 700 px | blurred 450, 550, 700 px |
| --- | --- | --- | --- | --- |
| 255 | `0,255,0,255,0,255` | 300, 300, 300 | 300, 300, 300 | 300, 300, 300 |
| 200 | `55,255,55,255,55,255` | 300, 300, 300 | 300, 300, 300 | 300, 300, 300 |
| 170 | `85,255,85,255,85,255` | 300, 300, 300 | 300, 300, 300 | 300, 300, 300 |
| 150 | `105,255,105,255,105,255` | 300, 300, 300 | 300, 300, 300 | 300, 300, 300 |
| 120 | `135,255,135,255,135,255` | 300, 300, 300 | 300, 300, 300 | 300, 300, 300 |
| 100 | `155,255,155,255,155,255` | 300, 300, 300 | 300, 300, 300 | 298, 300, 300 |
| 80 | `175,255,175,255,175,255` | 300, 300, 300 | 300, 300, 300 | 283, 291, 298 |

So 120 is the smallest span at which every code was read in every camera and at every width, and it is the floor of the contract. Spans of 150, 120 and 100 placed with dark at 0 (`0,150,...`) and in the middle (`52,202,...`) gave the same counts as with light at 255, to within three codes in the blurred camera at span 100. Palettes with different spans in the three channels:

| Levels | Spans (red, green, blue) | normal | harsh | blurred 450, 550, 700 px |
| --- | --- | --- | --- | --- |
| `60,255,0,200,90,255` | 195, 200, 165 | all | all | 300, 300, 300 |
| `105,255,0,255,105,255` | 150, 255, 150 | all | all | 300, 300 (700 not measured) |
| `100,220,100,220,0,255` | 120, 120, 255 | all | all | 249, 299, 300 |
| `0,255,52,202,0,255` | 255, 150, 255 | all | all | 283, 300 (700 not measured) |
| `0,255,60,180,0,255` | 255, 120, 255 | all | all | 269, 300, 300 |

That is where "green carries the most" comes from: a green with less contrast than both of its neighbours is the one case that lost codes above the floor, and only in the out-of-focus camera with the code small in the picture. Reading one color picture takes 16 to 21 ms on one core (450 to 700 pixels), against 4 ms for a black and white one. What these made-up cameras do not have: the blocks and ringing of a webcam's video compression, the moire between screen pixels and sensor pixels, glare, and a white balance that drifts. All of them cost contrast, which is why 150 is the recommendation and 120 the floor.

`scripts/flash_reference.py` is the reference encoder and decoder. Both implementations must reproduce its vector: session `0x01020304`, payload byte `i` = `(i*7+3) mod 256` for 1280 bytes, `c` = 300, so `k` = 5. The chunk sets of frames 5 to 12 are `[1] [0,3] [1] [0,3,4] [1,2,4] [1] [1,2,3] [2,4]`, and the SHA-256 of frames 0 to 39 concatenated is `16e659e1a7b1a24735a6f04aeca9514d3ab1a59f285993c2b1e6e97a00c39b12`.

The computer accepts a flash from a paired phone, or from a phone that has scanned a still-valid pairing code (the payload opens with that offer's flash key, which pairs the phone by its `phone.id`). Flash items are merged like Link items: the phone is the source of truth. That makes a flash only as current as the moment it was made, so a sender keeps it current while it is shown: the iPhone remakes its codes while the Flash screen is showing when something the shown flash carries is edited or deleted, a clip is removed, or a deletion is now kept on the computer. New items go in the next flash and do not restart the reading. Otherwise a flash made before an edit or a deletion that a Link sync then delivered would put the older version back. A known gap, not yet closed: if a Link sync delivers a deletion while an older flash is still being read, and the computer then finishes that older flash, the deleted item can come back on the computer. Clips in a flash are stored like any clip and never go on the computer's clipboard. A library that could not take an opened flash (busy) is tried again three seconds later while the phone still shows it; nothing of the failed try was kept (see Stored once).

A phone that paired by Flash has never shown the computer its iroh endpoint id. On its first Link connection it sends `pair` with the same secret, as after any scan. The computer accepts that secret once more, only from a `hello` whose `phone.id` is the one the flash carried, and from then on knows the phone by its endpoint id. Without `pair` that first connection is `refused not_paired`.

## Carrier 3: Backup folder

Zone can already keep a backup in any Files location (iCloud Drive, OneDrive, Dropbox). The package is a folder named `<something>.zonebackup` containing `library.json`:

```json
{ "formatVersion": 1, "createdAt": <seconds since 2001-01-01 UTC>,
  "notes": [ {"id","text","createdAt","editedAt"?} ],
  "dictations": [ {"id","text","createdAt","updatedAt","audioSeconds","engine","source","status", ...} ],
  "meetings": [ {"id","title","createdAt","updatedAt","status","engine","segments","speakers", ...} ] }
```

All dates in `library.json` are seconds since 2001-01-01 00:00:00 UTC (add 978307200 for Unix seconds). `zone import <path>` reads it once; `zone backup-folder <path>` makes the running daemon re-import whenever `library.json` changes. Imported items use the same ids as the phone, so the backup and the Link merge into one set of rows.
