import { toFull } from "../lib/ai";
import { listItems } from "../lib/zone";

type Input = {
  /**
   * The most characters of transcript to return, 1000 to 100000. Default 40000.
   */
  maxChars?: number;
};

/**
 * Get the most recent meeting recorded in Zone: its title, time, length, speakers and the
 * transcript, one line per speaker turn with the time into the meeting. For an older meeting,
 * use list-zone-items or search-zone with kinds ["meeting"], then get-zone-items.
 */
export default async function tool(input: Input) {
  const page = await listItems({ kinds: ["meeting"] }, 1);
  const meeting = page.items[0];
  if (!meeting) return { meeting: null, note: "No meetings have been recorded yet." };
  const max = Math.min(Math.max(input.maxChars ?? 40000, 1000), 100000);
  return { meeting: toFull(meeting, max), meetings: page.total };
}
