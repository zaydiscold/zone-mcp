import { clampLimit, nowLocal, parseKinds, toCard, withoutRecordings } from "../lib/ai";
import { listItems } from "../lib/agent-zone";

type Input = {
  /**
   * Only items created at or after this time. Accepts 2026-11-11, "2026-11-11 14:30" (local
   * time), RFC 3339 like 2026-11-11T14:30:00-08:00, an age like 2h, 3d or 1w, or today / yesterday.
   * Use the `now` field of an earlier answer to work out dates such as "Wednesday".
   */
  since?: string;
  /**
   * Only items created before this time, in the same formats as since. A bare date means through
   * the end of that day.
   */
  until?: string;
  /**
   * Only these kinds: "dictation", "note", "meeting", "clip". Leave empty for all.
   */
  kinds?: string[];
  /**
   * "newest" (default) or "oldest" first.
   */
  order?: "newest" | "oldest";
  /**
   * How many items to return, 1 to 100. Default 30.
   */
  limit?: number;
};

/**
 * List the person's Zone items in a time window, newest or oldest first, as short cards with
 * the total that match. Use it for questions about a time ("what did I say yesterday
 * afternoon"). Call get-zone-items with the ids for full text.
 */
export default async function tool(input: Input) {
  const kinds = parseKinds(input.kinds);
  const page = await listItems(
    {
      kinds,
      since: input.since || undefined,
      until: input.until || undefined,
      order: input.order === "oldest" ? "oldest" : "newest",
    },
    clampLimit(input.limit, 30, 100),
  );
  return {
    now: nowLocal(),
    total: page.total,
    more: page.hasMore,
    items: withoutRecordings(page.items, kinds).map(toCard),
  };
}
