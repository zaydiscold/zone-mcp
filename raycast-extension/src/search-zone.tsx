import { Action, ActionPanel, Icon, List } from "@raycast/api";
import { useState } from "react";

import { ZoneListItem } from "./components/ItemActions";
import { ZoneErrorView } from "./components/ZoneEmptyView";
import { openDashboard } from "./lib/actions";
import { groupByDay, plural } from "./lib/format";
import { KIND_FILTERS, useDebounced, useShowingDetail, useZoneItems } from "./lib/hooks";
import { Kind } from "./lib/zone";

export default function SearchZone() {
  const [searchText, setSearchText] = useState("");
  const [kind, setKind] = useState<"all" | Kind>("all");
  const query = useDebounced(searchText, 200);
  const { showingDetail, toggleDetail } = useShowingDetail("search-zone");
  const { data, isLoading, error, pagination, revalidate, mutate } = useZoneItems({
    query,
    filter: { kinds: kind === "all" ? undefined : [kind] },
    foldRecordings: kind === "all" || kind === "dictation",
  });

  const items = data ?? [];
  const groups = groupByDay(items);
  const searching = query.trim().length > 0;

  return (
    <List
      isLoading={isLoading || searchText !== query}
      isShowingDetail={showingDetail && items.length > 0}
      filtering={false}
      onSearchTextChange={setSearchText}
      searchBarPlaceholder="Search what you said and sent"
      pagination={pagination}
      searchBarAccessory={
        <List.Dropdown tooltip="Kind" storeValue onChange={(value) => setKind(value as "all" | Kind)}>
          {KIND_FILTERS.map((filter) => (
            <List.Dropdown.Item key={filter.value} title={filter.title} value={filter.value} />
          ))}
        </List.Dropdown>
      }
    >
      {error ? (
        <ZoneErrorView error={error} onRetry={revalidate} />
      ) : (
        !isLoading && (
          <List.EmptyView
            icon={searching ? Icon.MagnifyingGlass : { source: "icon.png" }}
            title={searching ? "No Matches" : "Nothing Here Yet"}
            description={
              searching
                ? "Try fewer words, word* for the beginning of a word, or another kind."
                : "When your iPhone syncs with zone, what you dictate shows up here. Pair a phone from Zone Status."
            }
            actions={
              <ActionPanel>
                <Action title="Open Zone Page" icon={Icon.Globe} onAction={() => openDashboard("/history")} />
              </ActionPanel>
            }
          />
        )
      )}
      {groups.map((group) => (
        <List.Section key={group.key} title={group.title} subtitle={plural(group.items.length, "item")}>
          {group.items.map((item) => (
            <ZoneListItem
              key={item.id}
              item={item}
              showingDetail={showingDetail}
              onToggleDetail={toggleDetail}
              onRefresh={revalidate}
              onDoneChanged={(changed, done) =>
                mutate(Promise.resolve(), {
                  optimisticUpdate: (current) =>
                    (current ?? []).map((one) =>
                      one.id === changed.id ? { ...one, processed_at: done ? Date.now() / 1000 : null } : one,
                    ),
                  shouldRevalidateAfter: false,
                })
              }
            />
          ))}
        </List.Section>
      ))}
    </List>
  );
}
