import { Color, Icon, Image, List } from "@raycast/api";

import { Kind, Segment, ZoneItem } from "./zone";

export const KIND_LABEL: Record<Kind, string> = {
  dictation: "Dictation",
  note: "Note",
  meeting: "Meeting",
  clip: "Clip",
};

export const KIND_PLURAL: Record<Kind, string> = {
  dictation: "Dictations",
  note: "Notes",
  meeting: "Meetings",
  clip: "Clips",
};

export const KIND_COLOR: Record<Kind, Color> = {
  dictation: Color.Blue,
  note: Color.Yellow,
  meeting: Color.Green,
  clip: Color.Purple,
};

const KIND_ICON: Record<Kind, Image.ImageLike> = {
  dictation: { source: Icon.Microphone, tintColor: KIND_COLOR.dictation },
  note: { source: Icon.Document, tintColor: KIND_COLOR.note },
  meeting: { source: Icon.TwoPeople, tintColor: KIND_COLOR.meeting },
  clip: { source: Icon.Clipboard, tintColor: KIND_COLOR.clip },
};

export function kindIcon(item: Pick<ZoneItem, "kind" | "processed_at">): Image.ImageLike {
  if (item.kind === "note" && item.processed_at) return { source: Icon.CheckCircle, tintColor: Color.SecondaryText };
  return KIND_ICON[item.kind] ?? Icon.Dot;
}

const SOURCE_LABEL: Record<string, string> = {
  keyboard: "Keyboard",
  app: "In Zone",
  note: "Note Recording",
  clipboard: "Clipboard",
  share: "Shared",
  shortcut: "Shortcut",
  typed: "Typed",
};

export function sourceLabel(source: string | null): string | undefined {
  if (!source) return undefined;
  return SOURCE_LABEL[source] ?? source.charAt(0).toUpperCase() + source.slice(1);
}

/** A voice note is listed once: the recording behind it repeats the note's words. */
export function isNoteRecording(item: Pick<ZoneItem, "kind" | "source">): boolean {
  return item.kind === "dictation" && item.source === "note";
}

/** The local calendar day of an item, as zone printed it: `2026-11-19`. */
export function dayKey(item: Pick<ZoneItem, "created_local">): string {
  return item.created_local.slice(0, 10);
}

function localDayKey(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export function dayTitle(key: string, now = new Date()): string {
  const today = localDayKey(now);
  const yesterday = localDayKey(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1));
  if (key === today) return "Today";
  if (key === yesterday) return "Yesterday";
  const [year, month, day] = key.split("-").map(Number);
  const date = new Date(year, month - 1, day);
  return date.toLocaleDateString("en-US", {
    weekday: "long",
    month: "long",
    day: "numeric",
    year: year === now.getFullYear() ? undefined : "numeric",
  });
}

/** Items grouped by local day, in the order they came. */
export function groupByDay(items: ZoneItem[]): { key: string; title: string; items: ZoneItem[] }[] {
  const groups: { key: string; title: string; items: ZoneItem[] }[] = [];
  const now = new Date();
  for (const item of items) {
    const key = dayKey(item);
    const last = groups.at(-1);
    if (last && last.key === key) last.items.push(item);
    else groups.push({ key, title: dayTitle(key, now), items: [item] });
  }
  return groups;
}

export function itemDate(item: Pick<ZoneItem, "created_at">): Date {
  return new Date(item.created_at * 1000);
}

export function timeOfDay(item: Pick<ZoneItem, "created_at">): string {
  return itemDate(item).toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" });
}

export function fullDate(item: Pick<ZoneItem, "created_at">): string {
  return itemDate(item).toLocaleString("en-US", {
    weekday: "short",
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

export function duration(seconds: number | null | undefined): string | undefined {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return undefined;
  const total = Math.round(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
}

export function plural(count: number, one: string, many = `${one}s`): string {
  return `${count.toLocaleString("en-US")} ${count === 1 ? one : many}`;
}

/** How long an item is, the way that fits its kind. */
export function lengthLabel(item: ZoneItem): string {
  if (item.kind === "clip") return plural([...item.text].length, "character");
  return duration(item.duration_s) ?? plural(item.words, "word");
}

/** One line for a list row. */
export function rowTitle(item: ZoneItem): string {
  const source = item.kind === "meeting" && item.title ? item.title : item.text;
  const line = source.split("\n").find((part) => part.trim()) ?? "";
  const trimmed = line.trim().replace(/\s+/g, " ");
  if (trimmed) return trimmed.length > 120 ? `${trimmed.slice(0, 119)}…` : trimmed;
  return item.kind === "meeting" ? "Untitled Meeting" : `Empty ${KIND_LABEL[item.kind] ?? "Item"}`;
}

export function meetingTitle(item: ZoneItem): string {
  return item.title?.trim() || "Untitled Meeting";
}

export interface Turn {
  speaker: string;
  start?: number;
  text: string;
}

/** A meeting's turns: from its segments when it has them, else from its `Speaker: words` lines. */
export function meetingTurns(item: ZoneItem): Turn[] {
  const segments: Segment[] = Array.isArray(item.extra?.segments) ? item.extra.segments : [];
  if (segments.length > 0) {
    return segments
      .filter((segment) => segment.text?.trim())
      .map((segment) => ({
        speaker: segment.speaker?.trim() || "Speaker",
        start: segment.start,
        text: segment.text!.trim(),
      }));
  }
  return item.text
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => {
      const match = /^([^:\n]{1,40}):\s+(.*)$/.exec(line);
      return match ? { speaker: match[1].trim(), text: match[2] } : { speaker: "", text: line.trim() };
    });
}

export function speakers(item: ZoneItem): string[] {
  const names = new Set<string>();
  for (const turn of meetingTurns(item)) if (turn.speaker) names.add(turn.speaker);
  return [...names];
}

/** Shows text as written: Markdown characters are escaped so speech is never read as formatting. */
export function escapeMarkdown(text: string): string {
  return text.replace(/([\\`*_{}[\]<>()#+\-.!|~])/g, "\\$1");
}

function paragraphs(text: string): string {
  return text
    .split("\n")
    .map((line) => escapeMarkdown(line))
    .join("  \n");
}

/** The detail pane: the whole text, a meeting by speaker. */
export function detailMarkdown(item: ZoneItem): string {
  if (item.kind === "meeting") {
    const turns = meetingTurns(item)
      .map((turn) => {
        const at = turn.start !== undefined ? ` · ${duration(turn.start)}` : "";
        const who = turn.speaker ? `**${escapeMarkdown(turn.speaker)}**${at}\n\n` : "";
        return `${who}${paragraphs(turn.text)}`;
      })
      .join("\n\n");
    return `## ${escapeMarkdown(meetingTitle(item))}\n\n${turns}`;
  }
  if (item.kind === "clip") {
    const fence = item.text.includes("```") ? "~~~~" : "```";
    return `${fence}\n${item.text}\n${fence}`;
  }
  return paragraphs(item.text) || "_Empty_";
}

/** Copy as Markdown: what a person pastes into notes or a document. */
export function itemMarkdown(item: ZoneItem): string {
  const when = fullDate(item);
  if (item.kind === "meeting") {
    const length = duration(item.duration_s);
    const header = `# ${meetingTitle(item)}\n\n${when}${length ? ` · ${length}` : ""}`;
    const turns = meetingTurns(item)
      .map((turn) => {
        const at = turn.start !== undefined ? ` (${duration(turn.start)})` : "";
        return turn.speaker ? `**${turn.speaker}**${at}: ${turn.text}` : turn.text;
      })
      .join("\n\n");
    return `${header}\n\n${turns}\n`;
  }
  if (item.kind === "note") {
    const box = item.processed_at ? "[x]" : "[ ]";
    return `- ${box} ${item.text.replace(/\n+/g, " ").trim()} (${when})\n`;
  }
  const quoted = item.text
    .split("\n")
    .map((line) => (line ? `> ${line}` : ">"))
    .join("\n");
  return `**${KIND_LABEL[item.kind] ?? "Item"}**, ${when}\n\n${quoted}\n`;
}

export function itemAccessories(item: ZoneItem, options: { showDate?: boolean } = {}): List.Item.Accessory[] {
  const accessories: List.Item.Accessory[] = [];
  const source = item.kind === "note" || item.kind === "meeting" ? undefined : sourceLabel(item.source);
  if (source) accessories.push({ tag: source });
  accessories.push({ text: lengthLabel(item) });
  if (options.showDate) accessories.push({ date: itemDate(item), tooltip: fullDate(item) });
  else accessories.push({ text: timeOfDay(item), tooltip: fullDate(item) });
  return accessories;
}

export function relativeAge(seconds: number): string {
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${plural(minutes, "minute")} ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${plural(hours, "hour")} ago`;
  return `${plural(Math.round(hours / 24), "day")} ago`;
}
