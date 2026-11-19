import { Action, ActionPanel, Color, Detail, Icon, Keyboard } from "@raycast/api";

import { InstallActions, notFoundDescription } from "./components/ZoneEmptyView";
import { openDashboard } from "./lib/actions";
import { KIND_PLURAL, plural } from "./lib/format";
import { useZoneStatus } from "./lib/hooks";
import { isZoneNotFound, KINDS, ZoneStatus } from "./lib/zone";

function statusMarkdown(status: ZoneStatus): string {
  const lines: string[] = [];
  if (status.daemon_running) {
    lines.push("# zone Is Running");
  } else {
    lines.push(
      "# zone Is Not Running",
      "",
      "Your library can still be read here, but nothing new arrives from your iPhone until zone runs.",
      "Run `zone install` once in Terminal to start zone every time you log in.",
    );
  }
  lines.push("");
  if (status.phones.length === 0) {
    lines.push(
      "## No Phone Paired Yet",
      "",
      "Choose **Pair a Phone**, then scan the code in Zone on your iPhone: Settings > Computers.",
    );
  } else {
    lines.push("## Phones", "");
    for (const phone of status.phones) {
      const sync = phone.last_sync ? `last synced ${phone.last_sync}` : "not synced yet";
      lines.push(`- **${phone.name}**: ${sync}, ${plural(phone.items_received, "item")} received`);
    }
  }
  const counts = KINDS.filter((kind) => status.items.by_kind[kind])
    .map((kind) => `- ${KIND_PLURAL[kind]}: ${status.items.by_kind[kind]!.toLocaleString("en-US")}`)
    .join("\n");
  lines.push("", "## Library", "", counts || "Empty so far.");
  return lines.join("\n");
}

export default function ZoneStatusCommand() {
  const { data: status, isLoading, error, revalidate } = useZoneStatus();

  const refresh = (
    <Action
      title="Refresh"
      icon={Icon.ArrowClockwise}
      shortcut={Keyboard.Shortcut.Common.Refresh}
      onAction={revalidate}
    />
  );

  if (error) {
    const missing = isZoneNotFound(error);
    return (
      <Detail
        markdown={
          missing
            ? `# zone Is Not Installed\n\n${notFoundDescription(error)}`
            : `# zone Could Not Answer\n\n${error.message}`
        }
        actions={<ActionPanel>{missing ? <InstallActions /> : refresh}</ActionPanel>}
      />
    );
  }

  const running = status?.daemon_running ?? false;
  return (
    <Detail
      isLoading={isLoading}
      markdown={status ? statusMarkdown(status) : ""}
      metadata={
        status && (
          <Detail.Metadata>
            <Detail.Metadata.TagList title="zone">
              <Detail.Metadata.TagList.Item
                text={running ? "Running" : "Not Running"}
                color={running ? Color.Green : Color.Red}
              />
            </Detail.Metadata.TagList>
            <Detail.Metadata.Label title="Version" text={status.daemon?.version ?? status.computer.zone_version} />
            <Detail.Metadata.Label title="This Mac" text={status.computer.name} />
            <Detail.Metadata.Separator />
            <Detail.Metadata.Label
              title="Phones"
              text={status.phones.length ? status.phones.map((phone) => phone.name).join(", ") : "None paired"}
            />
            <Detail.Metadata.Label title="Last Link Sync" text={status.last_sync.link?.time ?? "Never"} />
            {status.last_sync.flash && <Detail.Metadata.Label title="Last Flash" text={status.last_sync.flash.time} />}
            {status.last_sync.backup_folder && (
              <Detail.Metadata.Label title="Last Backup Import" text={status.last_sync.backup_folder.time} />
            )}
            <Detail.Metadata.Separator />
            <Detail.Metadata.Label title="Items" text={status.items.total.toLocaleString("en-US")} />
            {KINDS.filter((kind) => status.items.by_kind[kind]).map((kind) => (
              <Detail.Metadata.Label
                key={kind}
                title={KIND_PLURAL[kind]}
                text={status.items.by_kind[kind]!.toLocaleString("en-US")}
              />
            ))}
            {status.items.newest && <Detail.Metadata.Label title="Newest Item" text={status.items.newest} />}
          </Detail.Metadata>
        )
      }
      actions={
        <ActionPanel>
          <Action title="Open Zone Page" icon={Icon.Globe} onAction={() => openDashboard()} />
          <Action title="Open Flash" icon={Icon.Camera} onAction={() => openDashboard("", true)} />
          <Action title="Pair a Phone" icon={Icon.Mobile} onAction={() => openDashboard("/pair")} />
          {!running && <Action.CopyToClipboard title="Copy Start Command" content="zone install" />}
          {refresh}
        </ActionPanel>
      }
    />
  );
}
