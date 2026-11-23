import { Action, ActionPanel, Icon, List, openExtensionPreferences } from "@raycast/api";

import { APP_STORE_URL, INSTALL_COMMAND, INSTALL_URL, isZoneNotFound, ZoneNotFoundError } from "../lib/zone";

export function InstallActions() {
  return (
    <>
      <Action.OpenInBrowser title="Open Install Page" url={INSTALL_URL} />
      <Action.OpenInBrowser title="Get Zone for iPhone" icon={Icon.Mobile} url={APP_STORE_URL} />
      <Action.CopyToClipboard title="Copy Install Command" content={INSTALL_COMMAND} />
      <Action title="Open Extension Preferences" icon={Icon.Gear} onAction={openExtensionPreferences} />
    </>
  );
}

export function notFoundDescription(error: ZoneNotFoundError): string {
  if (error.configuredPath) {
    return `No zone program at ${error.configuredPath}. Fix the zone Path preference, or clear it to look in the usual places.`;
  }
  return "zone keeps a copy of your Zone for iPhone library on this computer: dictations, voice notes, meetings and clips. Install it, then pair your phone with zone pair.";
}

/** The view for an error: zone missing, or zone saying no. */
export function ZoneErrorView({ error, onRetry }: { error: Error; onRetry?: () => void }) {
  if (isZoneNotFound(error)) {
    return (
      <List.EmptyView
        icon={{ source: "icon.png" }}
        title={error.configuredPath ? "zone Not Found" : "zone Is Not Installed"}
        description={notFoundDescription(error)}
        actions={
          <ActionPanel>
            <InstallActions />
          </ActionPanel>
        }
      />
    );
  }
  return (
    <List.EmptyView
      icon={Icon.Warning}
      title="zone Could Not Read the Library"
      description={error.message}
      actions={
        <ActionPanel>
          {onRetry && <Action title="Try Again" icon={Icon.ArrowClockwise} onAction={onRetry} />}
          <Action.CopyToClipboard title="Copy Error" content={error.message} />
          <Action title="Open Extension Preferences" icon={Icon.Gear} onAction={openExtensionPreferences} />
        </ActionPanel>
      }
    />
  );
}
