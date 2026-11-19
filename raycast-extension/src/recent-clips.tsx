import { Icon, List } from "@raycast/api";

import { ZoneListItem } from "./components/ItemActions";
import { ZoneErrorView } from "./components/ZoneEmptyView";
import { groupByDay, plural } from "./lib/format";
import { useShowingDetail, useZoneItems } from "./lib/hooks";

export default function RecentClips() {
  const { showingDetail, toggleDetail } = useShowingDetail("recent-clips");
  const { data, isLoading, error, pagination, revalidate } = useZoneItems({ filter: { kinds: ["clip"] } });
  const clips = data ?? [];

  return (
    <List
      isLoading={isLoading}
      isShowingDetail={showingDetail && clips.length > 0}
      searchBarPlaceholder="Filter clips"
      pagination={pagination}
    >
      {error ? (
        <ZoneErrorView error={error} onRetry={revalidate} />
      ) : (
        !isLoading && (
          <List.EmptyView
            icon={Icon.Clipboard}
            title="No Clips Yet"
            description="In Zone on your iPhone, choose Settings > Computers > Send text to your computer, or share to Zone from another app."
          />
        )
      )}
      {groupByDay(clips).map((group) => (
        <List.Section key={group.key} title={group.title} subtitle={plural(group.items.length, "clip")}>
          {group.items.map((clip) => (
            <ZoneListItem
              key={clip.id}
              item={clip}
              showingDetail={showingDetail}
              onToggleDetail={toggleDetail}
              onRefresh={revalidate}
            />
          ))}
        </List.Section>
      ))}
    </List>
  );
}
