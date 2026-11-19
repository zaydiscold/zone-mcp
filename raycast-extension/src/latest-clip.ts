import { getPreferenceValues, open, showHUD, showToast, Toast } from "@raycast/api";

import { pasteOrCopy, showZoneFailure } from "./lib/actions";
import { plural, relativeAge } from "./lib/format";
import { INSTALL_URL, isZoneNotFound, latestClip } from "./lib/zone";

export default async function LatestClip() {
  const { clipAction } = getPreferenceValues<Preferences.LatestClip>();
  try {
    const clip = await latestClip();
    if (!clip) {
      await showHUD("No clips yet: send text from Zone on your iPhone");
      return;
    }
    const what = `clip from ${relativeAge(clip.age_s)} (${plural(clip.length, "character")})`;
    await pasteOrCopy(clip.text, clipAction === "copy" ? "copy" : "paste", what);
  } catch (error) {
    if (isZoneNotFound(error)) {
      await showToast({
        style: Toast.Style.Failure,
        title: "zone Is Not Installed",
        message: "Install zone to receive clips from your iPhone.",
        primaryAction: { title: "Open Install Page", onAction: () => open(INSTALL_URL) },
      });
      return;
    }
    await showZoneFailure(error, "Could Not Read the Latest Clip");
  }
}
