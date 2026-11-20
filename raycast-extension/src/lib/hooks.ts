import { useCachedState, usePromise } from "@raycast/utils";
import { useEffect, useRef, useState } from "react";

import { isNoteRecording } from "./format";
import { getStatus, Kind, ListCursor, ListFilter, listItems, searchItems, ZoneItem, ZoneStatus } from "./zone";

export const PAGE_SIZE = 50;

export function useDebounced<T>(value: T, delayMs = 200): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

/** Whether the detail pane is shown, remembered per command. */
export function useShowingDetail(key: string, initial = true) {
  const [showing, setShowing] = useCachedState(`${key}-detail`, initial);
  return { showingDetail: showing, toggleDetail: () => setShowing((value) => !value) };
}

type Cursor = { list?: ListCursor; search?: string | null };

export interface ZoneQuery {
  /** Words to search for. Empty lists by time instead. */
  query?: string;
  filter: ListFilter;
  /** Leave out the recording behind each voice note, as zone's page does. */
  foldRecordings?: boolean;
}

/**
 * A page at a time from zone: `zone search` when there is a query, `zone list` otherwise. A new
 * query cancels the run of zone it replaces. Nothing is cached on disk: the words stay in zone.
 */
export function useZoneItems({ query = "", filter, foldRecordings }: ZoneQuery) {
  const abortable = useRef<AbortController | null>(null);
  const trimmed = query.trim();
  const key = JSON.stringify(filter);
  return usePromise(
    (words: string, filterKey: string, fold: boolean) =>
      async (options: {
        page: number;
        cursor?: Cursor;
      }): Promise<{ data: ZoneItem[]; hasMore: boolean; cursor?: Cursor }> => {
        const parsed = JSON.parse(filterKey) as ListFilter;
        const signal = abortable.current?.signal;
        let items: ZoneItem[];
        let hasMore: boolean;
        let cursor: Cursor;
        if (words) {
          const page = await searchItems(
            words,
            { ...parsed, order: parsed.order ?? "newest" },
            PAGE_SIZE,
            options.cursor?.search,
            signal,
          );
          items = page.items;
          hasMore = page.nextCursor !== null && page.items.length > 0;
          cursor = { search: page.nextCursor };
        } else {
          const page = await listItems(parsed, PAGE_SIZE, options.cursor?.list, signal);
          items = page.items;
          hasMore = page.hasMore;
          cursor = { list: page.cursor };
        }
        return { data: fold ? items.filter((item) => !isNoteRecording(item)) : items, hasMore, cursor };
      },
    [trimmed, key, Boolean(foldRecordings)],
    { abortable },
  );
}

export function useZoneStatus() {
  return usePromise((): Promise<ZoneStatus> => getStatus());
}

export const KIND_FILTERS: { value: "all" | Kind; title: string }[] = [
  { value: "all", title: "All" },
  { value: "note", title: "Notes" },
  { value: "meeting", title: "Meetings" },
  { value: "dictation", title: "Dictations" },
  { value: "clip", title: "Clips" },
];
