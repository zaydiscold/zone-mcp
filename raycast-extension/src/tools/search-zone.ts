import { clampLimit, nowLocal, parseKinds, toCard, withoutRecordings } from "../lib/ai";
import { searchItems } from "../lib/zone";

type Input = {
  /**
   * Words to find. Every word must appear. Use "double quotes" for an exact phrase, OR between
   * alternatives, and a trailing * for the beginning of a word (transcri*). Case and accents do
   * not matter. Speech recognition can misspell names, so try a shorter beginning with * when a
   * name finds nothing.
   */
  query: string;
  /**
   * Only these kinds: "dictation", "note", "meeting", "clip". Leave empty for all.
   */
  kinds?: string[];
  /**
   * Only items created at or after this time. Accepts 2026-11-11, "2026-11-11 14:30" (local
   * time), RFC 3339 like 2026-11-11T14:30:00-08:00, an age like 2h, 3d or 1w, or today / yesterday.
   */
  since?: string;
  /**
   * Only items created before this time, in the same formats as since. A bare date means through
   * the end of that day.
   */
  until?: string;
  /**
   * How many matches to return, 1 to 50. Default 20.
   */
  limit?: number;
  /**
   * The nextCursor from an earlier call with the same other arguments, for the matches after it.
   */
  cursor?: string;
};

/**
 * Search the text of everything in the person's Zone library, newest first. Returns short cards
 * with a snippet around the match ([brackets] mark the matched words); call get-zone-items with
 * the ids for the full text.
 */
export default async function tool(input: Input) {
  const kinds = parseKinds(input.kinds);
  const page = await searchItems(
    input.query,
    { kinds, since: input.since || undefined, until: input.until || undefined, order: "newest" },
    clampLimit(input.limit, 20, 50),
    input.cursor || undefined,
  );
  return {
    now: nowLocal(),
    total: page.total,
    nextCursor: page.nextCursor,
    items: withoutRecordings(page.items, kinds).map(toCard),
  };
}
