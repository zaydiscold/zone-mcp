#!/usr/bin/env python3
"""Reference for the Zone Flash fountain code (PROTOCOL.md, carrier 2).

Frames are XORs of chunks chosen by a splitmix64 stream, so the receiver can rebuild the
payload from any k (plus a few) distinct frames, in any order, by solving over GF(2).
Run it to print the test vector both implementations must match.
"""
import hashlib, struct, sys

MASK = (1 << 64) - 1
MAGIC = b"ZF1"


def mix_words(session: int, index: int, count: int):
    state = ((session & 0xFFFFFFFF) << 32) | (index & 0xFFFFFFFF)
    for _ in range(count):
        state = (state + 0x9E3779B97F4A7C15) & MASK
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        yield z ^ (z >> 31)


def chunk_set(session: int, index: int, k: int):
    """Which chunks frame `index` XORs together, as a sorted list of chunk numbers."""
    if index < k:
        return [index]
    words = list(mix_words(session, index, (k + 63) // 64))
    chosen = [n for n in range(k) if (words[n // 64] >> (n % 64)) & 1]
    return chosen or [index % k]


def frame(session: int, payload: bytes, c: int, index: int) -> bytes:
    k = (len(payload) + c - 1) // c
    padded = payload + bytes(k * c - len(payload))
    body = bytearray(c)
    for n in chunk_set(session, index, k):
        part = padded[n * c:(n + 1) * c]
        for i in range(c):
            body[i] ^= part[i]
    return MAGIC + struct.pack(">IIHI", session, len(payload), k, index) + bytes(body)


def decode(frames):
    """Gaussian elimination over GF(2). Returns the payload, or None if more frames are needed."""
    rows = {}  # pivot chunk -> (bitmask of chunks, body as int)
    total = k = c = session = None
    for raw in frames:
        if raw[:3] != MAGIC:
            continue
        s, length, kk, index = struct.unpack(">IIHI", raw[3:17])
        body = raw[17:]
        if session is None:
            session, total, k, c = s, length, kk, len(body)
        if (s, length, kk, len(body)) != (session, total, k, c):
            continue
        mask = 0
        for n in chunk_set(s, index, k):
            mask |= 1 << n
        value = int.from_bytes(body, "big")
        while mask:
            pivot = mask.bit_length() - 1
            if pivot not in rows:
                rows[pivot] = (mask, value)
                break
            other_mask, other_value = rows[pivot]
            mask ^= other_mask
            value ^= other_value
        if len(rows) == k:
            break
    if k is None or len(rows) < k:
        return None
    solved = {}
    for pivot in sorted(rows):  # lowest pivot first: each row only mentions lower chunks
        mask, value = rows[pivot]
        rest = mask & ~(1 << pivot)
        while rest:
            n = rest.bit_length() - 1
            value ^= solved[n]
            rest &= ~(1 << n)
        solved[pivot] = value
    data = b"".join(solved[n].to_bytes(c, "big") for n in range(k))
    return data[:total]


if __name__ == "__main__":
    session = 0x01020304
    payload = bytes((i * 7 + 3) & 0xFF for i in range(1280))
    c = 300
    k = (len(payload) + c - 1) // c
    frames = [frame(session, payload, c, i) for i in range(40)]
    print("k =", k)
    print("chunk sets, frames 5..12:", [chunk_set(session, i, k) for i in range(5, 13)])
    print("frame 0 first 32 bytes:", frames[0][:32].hex())
    print("frame 7 first 32 bytes:", frames[7][:32].hex())
    print("sha256 of frames 0..39 concatenated:", hashlib.sha256(b"".join(frames)).hexdigest())
    assert decode(frames[:k]) == payload
    assert decode(frames[k:]) == payload, "must decode from coded frames alone"
    assert decode(list(reversed(frames[3:30]))) == payload
    big = bytes((i * 31 + 5) & 0xFF for i in range(400_000))
    kb = (len(big) + c - 1) // c
    bigframes = [frame(0xA1B2C3D4, big, c, i) for i in range(kb, 2 * kb + 20)]
    assert decode(bigframes) == big
    print("big: k =", kb, "decoded from", len(bigframes), "coded frames only")
    print("ok")
