import { clampLimit, toFull } from "../lib/ai";
import { showItems } from "../lib/agent-zone";

type Input = {
  /**
   * Item ids from search-zone, list-zone-items or another Zone tool. At most 20.
   */
  ids: string[];
  /**
   * The most characters of text to return per item, 200 to 50000. Default 20000. Long meetings
   * come back with truncated true when cut.
   */
  maxChars?: number;
};

/**
 * Get the full text of Zone items by id. A meeting comes back as one line per speaker turn with
 * the time into the meeting.
 */
export default async function tool(input: Input) {
  const ids = [...new Set((input.ids ?? []).map((id) => id.trim()).filter(Boolean))].slice(0, 20);
  const maxChars = clampLimit(input.maxChars, 20000, 50000);
  const { items, missing } = await showItems(ids);
  return { items: items.map((item) => toFull(item, Math.max(200, maxChars))), notFound: missing };
}
