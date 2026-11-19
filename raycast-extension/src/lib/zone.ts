import { getPreferenceValues } from "@raycast/api";

import { setSettingsSource, ZoneSettings } from "./cli";

export * from "./cli";

setSettingsSource(() => getPreferenceValues<ZoneSettings>());
