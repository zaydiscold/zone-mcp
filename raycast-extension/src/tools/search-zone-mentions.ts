import { Icon } from "@raycast/api";

import { withoutRecordings } from "../lib/ai";
import { fullDate, KIND_LABEL, rowTitle } from "../lib/format";
import { Kind, listItems, searchItems, ZoneItem } from "../lib/zone";

type Input = {
  /** What the person typed after choosing Zone Items, empty for the newest items. */
  query: string;
};

const ICON: Record<Kind, string> = {
  dictation: Icon.Microphone,
  note: Icon.Document,
  meeting: Icon.TwoPeople,
  clip: Icon.Clipboard,
};

const INLINE_CHARS = 3000;

function toMention(item: ZoneItem) {
  const mention: { title: string; uri: string; description: string; icon: string; content?: string } = {
    title: rowTitle(item),
    uri: `zone://item/${item.id}`,
    description: `${KIND_LABEL[item.kind] ?? item.kind}, ${fullDate(item)}`,
    icon: ICON[item.kind] ?? Icon.Dot,
  };
  if (item.text.length <= INLINE_CHARS) mention.content = item.text;
  return mention;
}

/**
 * Search Zone items to mention in AI Chat. Shows the newest items when the query is empty.
 */
export default async function tool(input: Input) {
  const query = input.query?.trim() ?? "";
  const items = query ? (await searchItems(query, { order: "newest" }, 25)).items : (await listItems({}, 25)).items;
  return withoutRecordings(items).map(toMention);
}
