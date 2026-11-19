import { Action, ActionPanel, Icon, List } from "@raycast/api";
import { useState } from "react";

import { ZoneListItem } from "./components/ItemActions";
import { ZoneErrorView } from "./components/ZoneEmptyView";
import { openDashboard } from "./lib/actions";
import { groupByDay, plural } from "./lib/format";
import { useShowingDetail, useZoneItems } from "./lib/hooks";

export default function OpenNotes() {
  const [order, setOrder] = useState<"newest" | "oldest">("newest");
  const { showingDetail, toggleDetail } = useShowingDetail("open-notes", false);
  const { data, isLoading, error, pagination, revalidate, mutate } = useZoneItems({
    filter: { kinds: ["note"], unprocessed: true, order },
  });
  const notes = data ?? [];

  return (
    <List
      isLoading={isLoading}
      isShowingDetail={showingDetail && notes.length > 0}
      searchBarPlaceholder="Filter open notes"
      pagination={pagination}
      searchBarAccessory={
        <List.Dropdown tooltip="Order" storeValue onChange={(value) => setOrder(value as "newest" | "oldest")}>
          <List.Dropdown.Item title="Newest First" value="newest" />
          <List.Dropdown.Item title="Oldest First" value="oldest" />
        </List.Dropdown>
      }
    >
      {error ? (
        <ZoneErrorView error={error} onRetry={revalidate} />
      ) : (
        !isLoading && (
          <List.EmptyView
            icon={Icon.CheckCircle}
            title="No Open Notes"
            description="Every voice note is done. New ones from Zone on your iPhone show up here."
            actions={
              <ActionPanel>
                <Action
                  title="Show Done Notes in Zone Page"
                  icon={Icon.Globe}
                  onAction={() => openDashboard("/notes?show=done")}
                />
              </ActionPanel>
            }
          />
        )
      )}
      {groupByDay(notes).map((group) => (
        <List.Section key={group.key} title={group.title} subtitle={plural(group.items.length, "note")}>
          {group.items.map((note) => (
            <ZoneListItem
              key={note.id}
              item={note}
              showingDetail={showingDetail}
              onToggleDetail={toggleDetail}
              onRefresh={revalidate}
              onDoneChanged={(changed) =>
                mutate(Promise.resolve(), {
                  optimisticUpdate: (current) => (current ?? []).filter((one) => one.id !== changed.id),
                  shouldRevalidateAfter: false,
                })
              }
              extra={
                <Action
                  title="Open Notes in Zone Page"
                  icon={Icon.List}
                  onAction={() => openDashboard(`/notes?order=${order}`)}
                />
              }
            />
          ))}
        </List.Section>
      ))}
    </List>
  );
}
