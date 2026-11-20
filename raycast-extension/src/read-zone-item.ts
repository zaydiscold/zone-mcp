import { itemMarkdown } from "./lib/format";
import { showItems } from "./lib/zone";

type Input = {
  /** The `uri` of an item a Zone mention search returned: zone://item/<id>. */
  uri: string;
};

/** Reads one Zone item, mentioned in AI Chat, as Markdown. */
export default async function readZoneItem(input: Input): Promise<string> {
  const id = input.uri.replace(/^zone:\/\/item\//, "").trim();
  const { items } = await showItems([id]);
  const item = items[0];
  if (!item) return `This Zone item (${id}) is no longer in the library.`;
  return itemMarkdown(item);
}
