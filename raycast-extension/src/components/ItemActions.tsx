import { Action, ActionPanel, Icon, Keyboard, List } from "@raycast/api";
import { ReactNode } from "react";

import { markDone, openDashboard } from "../lib/actions";
import {
  detailMarkdown,
  fullDate,
  itemAccessories,
  itemMarkdown,
  KIND_COLOR,
  KIND_LABEL,
  kindIcon,
  lengthLabel,
  meetingTurns,
  rowTitle,
  sourceLabel,
  speakers,
} from "../lib/format";
import { itemRoute, ZoneItem } from "../lib/zone";

const primaryModifier = process.platform === "win32" ? "ctrl" : "cmd";

export interface ItemActionsProps {
  item: ZoneItem;
  /** Called after Mark Done or Mark Not Done changed the item. */
  onDoneChanged?: (item: ZoneItem, done: boolean) => void;
  onToggleDetail?: () => void;
  onRefresh?: () => void;
  extra?: ReactNode;
}

export function transcriptText(item: ZoneItem): string {
  if (item.kind !== "meeting") return item.text;
  return meetingTurns(item)
    .map((turn) => (turn.speaker ? `${turn.speaker}: ${turn.text}` : turn.text))
    .join("\n");
}

export function ItemActions({ item, onDoneChanged, onToggleDetail, onRefresh, extra }: ItemActionsProps) {
  const text = transcriptText(item);
  const isMeeting = item.kind === "meeting";
  return (
    <ActionPanel>
      <ActionPanel.Section>
        <Action.Paste title="Paste to Active App" content={text} />
        <Action.CopyToClipboard title={isMeeting ? "Copy Transcript" : "Copy Text"} content={text} />
        <Action.CopyToClipboard
          title="Copy as Markdown"
          content={itemMarkdown(item)}
          shortcut={Keyboard.Shortcut.Common.Copy}
        />
      </ActionPanel.Section>
      <ActionPanel.Section>
        {item.kind === "note" &&
          (item.processed_at ? (
            <Action
              title="Mark as Not Done"
              icon={Icon.Circle}
              shortcut={{ modifiers: [primaryModifier], key: "d" }}
              onAction={async () => {
                if (await markDone([item], false)) onDoneChanged?.(item, false);
              }}
            />
          ) : (
            <Action
              title="Mark as Done"
              icon={Icon.CheckCircle}
              shortcut={{ modifiers: [primaryModifier], key: "d" }}
              onAction={async () => {
                if (await markDone([item], true)) onDoneChanged?.(item, true);
              }}
            />
          ))}
        <Action
          title="Open in Zone Page"
          icon={Icon.Globe}
          shortcut={Keyboard.Shortcut.Common.Open}
          onAction={() => openDashboard(itemRoute(item))}
        />
        {extra}
      </ActionPanel.Section>
      <ActionPanel.Section>
        {onToggleDetail && (
          <Action
            title="Toggle Details"
            icon={Icon.AppWindowSidebarRight}
            shortcut={{ modifiers: [primaryModifier, "shift"], key: "d" }}
            onAction={onToggleDetail}
          />
        )}
        <Action.CopyToClipboard title="Copy ID" content={item.id} shortcut={Keyboard.Shortcut.Common.CopyName} />
        {onRefresh && (
          <Action
            title="Refresh"
            icon={Icon.ArrowClockwise}
            shortcut={Keyboard.Shortcut.Common.Refresh}
            onAction={onRefresh}
          />
        )}
      </ActionPanel.Section>
    </ActionPanel>
  );
}

export function ItemDetail({ item }: { item: ZoneItem }) {
  const source = sourceLabel(item.source);
  const who = item.kind === "meeting" ? speakers(item) : [];
  return (
    <List.Item.Detail
      markdown={detailMarkdown(item)}
      metadata={
        <List.Item.Detail.Metadata>
          <List.Item.Detail.Metadata.TagList title="Kind">
            <List.Item.Detail.Metadata.TagList.Item
              text={KIND_LABEL[item.kind] ?? item.kind}
              color={KIND_COLOR[item.kind]}
            />
          </List.Item.Detail.Metadata.TagList>
          <List.Item.Detail.Metadata.Label title="When" text={fullDate(item)} />
          {source && <List.Item.Detail.Metadata.Label title="Source" text={source} />}
          <List.Item.Detail.Metadata.Label title="Length" text={lengthLabel(item)} />
          {item.kind !== "clip" && <List.Item.Detail.Metadata.Label title="Words" text={String(item.words)} />}
          {who.length > 0 && <List.Item.Detail.Metadata.Label title="Speakers" text={who.join(", ")} />}
          {item.kind === "note" && (
            <List.Item.Detail.Metadata.Label title="Done" text={item.processed_at ? "Yes" : "No"} />
          )}
        </List.Item.Detail.Metadata>
      }
    />
  );
}

export interface ZoneListItemProps extends ItemActionsProps {
  showingDetail: boolean;
  showDate?: boolean;
  title?: string;
  subtitle?: string;
}

export function ZoneListItem(props: ZoneListItemProps) {
  const { item, showingDetail, showDate, title, subtitle } = props;
  return (
    <List.Item
      id={item.id}
      icon={{ value: kindIcon(item), tooltip: KIND_LABEL[item.kind] ?? item.kind }}
      title={title ?? rowTitle(item)}
      subtitle={showingDetail ? undefined : subtitle}
      accessories={showingDetail ? undefined : itemAccessories(item, { showDate })}
      detail={showingDetail ? <ItemDetail item={item} /> : undefined}
      actions={<ItemActions {...props} />}
    />
  );
}
