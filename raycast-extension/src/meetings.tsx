import { Icon, List } from "@raycast/api";
import { useState } from "react";

import { ZoneListItem } from "./components/ItemActions";
import { ZoneErrorView } from "./components/ZoneEmptyView";
import { meetingTitle, speakers } from "./lib/format";
import { useDebounced, useShowingDetail, useZoneItems } from "./lib/hooks";

export default function Meetings() {
  const [searchText, setSearchText] = useState("");
  const query = useDebounced(searchText, 200);
  const { showingDetail, toggleDetail } = useShowingDetail("meetings", false);
  const { data, isLoading, error, pagination, revalidate } = useZoneItems({ query, filter: { kinds: ["meeting"] } });
  const meetings = data ?? [];
  const searching = query.trim().length > 0;

  return (
    <List
      isLoading={isLoading || searchText !== query}
      isShowingDetail={showingDetail && meetings.length > 0}
      filtering={false}
      onSearchTextChange={setSearchText}
      searchBarPlaceholder="Search meetings"
      pagination={pagination}
    >
      {error ? (
        <ZoneErrorView error={error} onRetry={revalidate} />
      ) : (
        !isLoading && (
          <List.EmptyView
            icon={searching ? Icon.MagnifyingGlass : Icon.TwoPeople}
            title={searching ? "No Matching Meetings" : "No Meetings Yet"}
            description={
              searching
                ? "Try other words."
                : "Meetings you record in Zone on your iPhone show up here with transcripts."
            }
          />
        )
      )}
      {meetings.map((meeting) => {
        const who = speakers(meeting);
        return (
          <ZoneListItem
            key={meeting.id}
            item={meeting}
            title={meetingTitle(meeting)}
            subtitle={who.join(", ")}
            showDate
            showingDetail={showingDetail}
            onToggleDetail={toggleDetail}
            onRefresh={revalidate}
          />
        );
      })}
    </List>
  );
}
