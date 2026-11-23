import { clampLimit, toFull } from "../lib/ai";
import { listItems } from "../lib/agent-zone";

type Input = {
  /**
   * "newest" (default) or "oldest" first.
   */
  order?: "newest" | "oldest";
  /**
   * How many notes to return, 1 to 100. Default 50.
   */
  limit?: number;
};

/**
 * Get the person's voice notes that are not marked done yet: their to-do list, with full text.
 * Notes are spoken, so expect recognition mistakes. Use mark-notes-done after a note is handled,
 * only when the person asks for it.
 */
export default async function tool(input: Input) {
  const page = await listItems(
    { kinds: ["note"], unprocessed: true, order: input.order === "oldest" ? "oldest" : "newest" },
    clampLimit(input.limit, 50, 100),
  );
  return { open: page.total, more: page.hasMore, notes: page.items.map((note) => toFull(note, 4000)) };
}
