// Tests for src/lib/cli.ts. The pure parts always run. The parts that run zone need a throwaway
// library filled with synthetic items, never a real one:
//
//   ZONE_TEST_BIN=/path/to/zone ZONE_TEST_HOME=/tmp/zone-demo npm test
//
// (README.md, "Testing", shows how to fill one with `zone dev synthetic --demo`.)
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  agentRead,
  runZone,
  findZone,
  listItems,
  searchItems,
  setDone,
  setSettingsSource,
  showItems,
  timeArg,
  ZoneCommandError,
  ZoneNotFoundError,
} from "../src/lib/cli.ts";

const bin = process.env.ZONE_TEST_BIN;
const home = process.env.ZONE_TEST_HOME;
const live = bin && home ? {} : { skip: "set ZONE_TEST_BIN and ZONE_TEST_HOME to a throwaway library" };

test("timeArg writes UTC with microseconds", () => {
  assert.equal(timeArg(1795110613.337087), "2026-11-19T17:50:13.337087Z");
  assert.equal(timeArg(1795110613), "2026-11-19T17:50:13.000000Z");
  assert.equal(timeArg(1795110613.9999999), "2026-11-19T17:50:14.000000Z");
});

test("a missing zone path is reported as not found", async () => {
  setSettingsSource(() => ({ zonePath: "/nonexistent/zone" }));
  await assert.rejects(listItems({}, 1), (error: unknown) => error instanceof ZoneNotFoundError);
});

test(
  "Windows automatically discovers the installer copy",
  { skip: process.platform !== "win32" || !process.env.ZONE_TEST_WINDOWS_INSTALL },
  async () => {
    setSettingsSource(() => ({ libraryFolder: home }));
    assert.equal(findZone(), process.env.ZONE_TEST_WINDOWS_INSTALL);
    assert.ok((await listItems({}, 1)).total > 0);
  },
);

test("list pages by time cover every item once", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  for (const order of ["newest", "oldest"] as const) {
    const all = await listItems({ order }, 100000);
    const seen: string[] = [];
    let cursor;
    for (let guard = 0; guard < 1000; guard++) {
      const page = await listItems({ order }, 37, cursor);
      seen.push(...page.items.map((item) => item.id));
      if (!page.hasMore) break;
      cursor = page.cursor;
    }
    assert.equal(new Set(seen).size, seen.length, `${order}: no item twice`);
    assert.deepEqual(
      seen,
      all.items.map((item) => item.id),
      `${order}: same items in the same order`,
    );
  }
});

test("open notes page by time too", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  const first = await listItems({ kinds: ["note"], unprocessed: true }, 10);
  assert.ok(first.items.every((item) => item.kind === "note" && item.processed_at === null));
  assert.ok(first.total >= first.items.length);
});

test("search pages with zone's cursor, and a leading dash is a word", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  const page = await searchItems("the", { order: "newest" }, 5);
  assert.ok(page.items.length > 0);
  if (page.nextCursor) {
    const next = await searchItems("the", { order: "newest" }, 5, page.nextCursor);
    const ids = new Set(page.items.map((item) => item.id));
    assert.ok(next.items.every((item) => !ids.has(item.id)));
  }
  await searchItems("-lantern", {}, 5);
});

test("show reports unknown ids instead of failing", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  const [one] = (await listItems({}, 1)).items;
  const result = await showItems([one.id, "00000000-NOPE"]);
  assert.deepEqual(
    result.items.map((item) => item.id),
    [one.id],
  );
  assert.deepEqual(result.missing, ["00000000-NOPE"]);
});

test("done is set and cleared through zone mcp", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  const [note] = (await listItems({ kinds: ["note"], unprocessed: true }, 1)).items;
  assert.ok(note, "the library needs an open note");
  assert.equal((await setDone([note.id], true)).updated, 1);
  assert.notEqual((await showItems([note.id])).items[0].processed_at, null);
  assert.equal((await setDone([note.id], false)).updated, 1);
  assert.equal((await showItems([note.id])).items[0].processed_at, null);
});

test("zone's own errors come through as ZoneCommandError", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  await assert.rejects(listItems({ since: "not a time" }, 1), (error: unknown) => error instanceof ZoneCommandError);
});

test("Raycast AI respects the switch while manual reads remain available", live, async () => {
  setSettingsSource(() => ({ zonePath: bin, libraryFolder: home }));
  const read = agentRead(() => listItems({}, 1));
  try {
    assert.ok((await read()).total > 0);
    await runZone(["agents", "off"]);
    await assert.rejects(read(), /access is off/i);
    assert.ok((await listItems({}, 1)).total > 0);
    await runZone(["agents", "on"]);
    const interrupted = agentRead(async () => {
      const page = await listItems({}, 1);
      await runZone(["agents", "off"]);
      return page;
    });
    await assert.rejects(interrupted(), /access is off/i);
  } finally {
    await runZone(["agents", "on"]);
  }
});
