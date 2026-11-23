import { Tool } from "@raycast/api";

import { plural } from "../lib/format";
import { setDone, showItems } from "../lib/agent-zone";

type Input = {
  /**
   * Ids of voice notes (kind "note") from get-open-notes, search-zone or list-zone-items. At
   * most 50.
   */
  ids: string[];
  /**
   * true (default) marks them done; false marks them not done again.
   */
  done?: boolean;
};

async function notesFor(ids: string[]) {
  const unique = [...new Set(ids.map((id) => id.trim()).filter(Boolean))].slice(0, 50);
  const { items, missing } = await showItems(unique);
  const notes = items.filter((item) => item.kind === "note");
  const notNotes = items.filter((item) => item.kind !== "note").map((item) => item.id);
  return { notes, missing, notNotes };
}

export const confirmation: Tool.Confirmation<Input> = async (input) => {
  const { notes } = await notesFor(input.ids ?? []);
  const done = input.done ?? true;
  return {
    message: `Mark ${plural(notes.length, "note")} as ${done ? "done" : "not done"}?`,
    info: notes.slice(0, 10).map((note) => ({
      name: note.created_local.slice(0, 16).replace("T", " "),
      value: note.text.length > 120 ? `${note.text.slice(0, 119)}…` : note.text,
    })),
  };
};

/**
 * Mark voice notes as done (or not done again), the same Done as in Zone's page. It only sets
 * this flag; it never changes or deletes any text. Only call it when the person asks to tick
 * notes off.
 */
export default async function tool(input: Input) {
  const done = input.done ?? true;
  const { notes, missing, notNotes } = await notesFor(input.ids ?? []);
  if (notes.length === 0) return { updated: 0, notFound: missing, notNotes };
  const result = await setDone(
    notes.map((note) => note.id),
    done,
  );
  return { updated: result.updated, done, notFound: [...missing, ...result.not_found], notNotes };
}
