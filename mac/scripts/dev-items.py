#!/usr/bin/env python3
"""Made-up things a person might have said, for trying Zone for Mac without a real library.

    dev-items.py backup FOLDER        write FOLDER/Demo.zonebackup, the way the iPhone app writes a backup
    dev-items.py items COUNT SEED     print COUNT items as JSON Lines, the way a phone sends them

Nothing here is anyone's real speech.
"""
import json
import os
import random
import sys
import time
import uuid

APPLE_EPOCH = 978307200

DICTATIONS = [
    "On my way, be there in ten minutes.",
    "Can we move the sync to three?",
    "Sounds good, I will send the slides tonight.",
    "Running five minutes late, start without me.",
    "Can you review the pricing page copy when you get a chance?",
    "Dinner at seven works. Should I bring anything?",
    "The build is green, merging now.",
    "Let's push the launch to Tuesday so we can test on more phones.",
    "Loved the photos from the trip, send me the one by the lake.",
    "Morning! Coffee at the usual place?",
    "Just talked to the landlord, the rent stays the same if we sign this week.",
    "Remind me to call the dentist after lunch.",
    "Yes, send it over and I will look tonight.",
    "Happy birthday! Hope the day is great.",
]

NOTES = [
    "Idea: a reading lamp that dims with the sunset. Warm at night, cool in the morning.",
    "Groceries: oat milk, lemons, the good bread, coffee filters.",
    "Pitch for the Thursday review: start with the customer story, then the numbers, then the ask.",
    "Trip to Portland: train on Friday morning, back Sunday night. Bring the rain jacket.",
    "Gift ideas for Sam: film camera, a good notebook, concert tickets.",
    "Workout plan: three runs a week, one long one on Sunday, stretch after every run.",
    "Start every morning by writing the one thing that matters most today.",
    "Lease: two years at the same rent if we sign by Friday. Need to read the pet clause.",
]

MEETING = [
    ("A", 0, 6, "Let's start with the launch date. Are we still on for Tuesday?"),
    ("B", 6, 14, "Yes, if the phone tests pass by Monday. I have three devices left to check."),
    ("A", 14, 20, "Good. Priya, where is the hiring plan?"),
    ("C", 20, 31, "The draft is done. Two engineers in Q4 and a designer in January."),
    ("B", 31, 38, "I will send the notes from this meeting to everyone after lunch."),
]


def new_id(rng):
    return str(uuid.UUID(int=rng.getrandbits(128), version=4)).upper()


def backup(folder):
    """A week of notes and dictations and one meeting, ending a few hours ago."""
    rng = random.Random(7)
    now = time.time()

    def ago(days, hours):
        return now - days * 86400 - hours * 3600 - APPLE_EPOCH

    library = {"formatVersion": 1, "createdAt": now - APPLE_EPOCH, "notes": [], "dictations": [], "meetings": []}
    for index, text in enumerate(NOTES):
        library["notes"].append({"id": new_id(rng), "text": text, "createdAt": ago(6 - index * 0.8, 3)})
    for index, text in enumerate(DICTATIONS):
        created = ago(6 - index * 0.45, 1.5)
        library["dictations"].append({
            "id": new_id(rng), "text": text, "createdAt": created, "updatedAt": created,
            "audioSeconds": round(len(text) / 14, 1), "engine": "parakeet", "source": "keyboard", "status": "inserted",
        })
    library["meetings"].append({
        "id": new_id(rng), "title": "Launch planning", "createdAt": ago(4, 0.5), "updatedAt": ago(4, 0),
        "status": "finished", "engine": "parakeet",
        "speakers": [{"id": "A", "name": "Me", "isMe": True}, {"id": "B", "name": "Jordan"}, {"id": "C", "name": "Priya"}],
        "segments": [{"speaker": s, "start": a, "end": b, "text": x} for s, a, b, x in MEETING],
    })
    target = os.path.join(folder, "Demo.zonebackup")
    os.makedirs(target, exist_ok=True)
    with open(os.path.join(target, "library.json"), "w") as out:
        json.dump(library, out, indent=1)


def items(count, seed):
    """Items said over the last few minutes, oldest first."""
    rng = random.Random(seed)
    now = time.time()
    recent = []
    for index in range(count):
        is_note = rng.random() < 0.2
        pool = NOTES if is_note else DICTATIONS
        # No line twice among the last few, so a short batch never reads as a stutter.
        fresh = [line for line in pool if line not in recent] or pool
        text = rng.choice(fresh)
        recent = (recent + [text])[-4:]
        created = now - (count - index) * 7
        print(json.dumps({
            "id": new_id(rng),
            "kind": "note" if is_note else "dictation",
            "source": None if is_note else "keyboard",
            "created_at": created,
            "updated_at": created,
            "text": text,
            "title": None,
            "duration_s": None if is_note else round(len(text) / 14, 1),
            "engine": None if is_note else "parakeet",
            "status": None if is_note else "inserted",
            "extra": {},
            "rev": "%016x" % rng.getrandbits(64),
        }))


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "backup":
        backup(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "items":
        items(int(sys.argv[2]), int(sys.argv[3]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
