import { duration, isNoteRecording, meetingTurns, speakers } from "./format";
import { Kind, KINDS, ZoneItem } from "./zone";

// Shapes handed to Raycast AI. Cards are short on purpose; full text comes from get-zone-items.

export const CARD_CHARS = 280;

export interface Card {
  id: string;
  kind: Kind;
  source: string | null;
  /** Local time with the UTC offset, as zone printed it. */
  time: string;
  title?: string;
  preview: string;
  truncated: boolean;
  length?: string;
  words: number;
  done?: boolean;
  snippet?: string;
}

export function toCard(item: ZoneItem): Card {
  const text = item.text.replace(/\s+/g, " ").trim();
  const card: Card = {
    id: item.id,
    kind: item.kind,
    source: item.source,
    time: item.created_local,
    preview: text.length > CARD_CHARS ? `${text.slice(0, CARD_CHARS)}…` : text,
    truncated: text.length > CARD_CHARS,
    words: item.words,
  };
  if (item.title) card.title = item.title;
  const length = duration(item.duration_s);
  if (length) card.length = length;
  if (item.kind === "note") card.done = item.processed_at !== null;
  if (item.snippet) card.snippet = item.snippet;
  return card;
}

export interface FullItem {
  id: string;
  kind: Kind;
  source: string | null;
  time: string;
  title?: string;
  length?: string;
  words: number;
  done?: boolean;
  speakers?: string[];
  text: string;
  truncated: boolean;
}

export function toFull(item: ZoneItem, maxChars: number): FullItem {
  let text = item.text;
  if (item.kind === "meeting") {
    text = meetingTurns(item)
      .map((turn) => {
        const at = turn.start !== undefined ? `[${duration(turn.start)}] ` : "";
        return turn.speaker ? `${at}${turn.speaker}: ${turn.text}` : `${at}${turn.text}`;
      })
      .join("\n");
  }
  const full: FullItem = {
    id: item.id,
    kind: item.kind,
    source: item.source,
    time: item.created_local,
    words: item.words,
    text: text.length > maxChars ? text.slice(0, maxChars) : text,
    truncated: text.length > maxChars,
  };
  if (item.title) full.title = item.title;
  const length = duration(item.duration_s);
  if (length) full.length = length;
  if (item.kind === "note") full.done = item.processed_at !== null;
  if (item.kind === "meeting") full.speakers = speakers(item);
  return full;
}

/** Kinds the AI asked for, keeping only the ones zone knows. */
export function parseKinds(kinds?: string[]): Kind[] | undefined {
  const known = (kinds ?? [])
    .map((kind) => kind.toLowerCase().replace(/s$/, ""))
    .filter((kind): kind is Kind => (KINDS as string[]).includes(kind));
  return known.length > 0 ? [...new Set(known)] : undefined;
}

export function clampLimit(limit: number | undefined, fallback: number, max: number): number {
  if (limit === undefined || !Number.isFinite(limit)) return fallback;
  return Math.min(Math.max(1, Math.floor(limit)), max);
}

/** The current local time, so the AI can turn "yesterday afternoon" into times. */
export function nowLocal(): string {
  const now = new Date();
  const offset = -now.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  const pad = (n: number) => String(Math.floor(Math.abs(n))).padStart(2, "0");
  const local = new Date(now.getTime() + offset * 60_000).toISOString().slice(0, 19);
  const weekday = now.toLocaleDateString("en-US", { weekday: "long" });
  return `${weekday} ${local}${sign}${pad(offset / 60)}:${pad(offset % 60)}`;
}

export function withoutRecordings(items: ZoneItem[], kinds?: Kind[]): ZoneItem[] {
  if (kinds && kinds.length === 1 && kinds[0] === "dictation") return items;
  return items.filter((item) => !isNoteRecording(item));
}
