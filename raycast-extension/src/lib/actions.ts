import { Clipboard, open, showHUD, showToast, Toast } from "@raycast/api";

import { isNotRunning, isZoneNotFound, pageUrl, setDone, ZoneItem } from "./zone";

export async function showZoneFailure(error: unknown, title: string) {
  const message = error instanceof Error ? error.message : String(error);
  const toast: Toast.Options = { style: Toast.Style.Failure, title, message };
  if (isNotRunning(error)) {
    toast.message = "zone is not running. Start it with zone install (or zone serve).";
    toast.primaryAction = {
      title: "Copy Start Command",
      onAction: () => Clipboard.copy("zone install"),
    };
  } else if (isZoneNotFound(error)) {
    toast.message = "zone is not installed, or the zone Path preference is wrong.";
  }
  await showToast(toast);
}

/** Opens zone's page in the browser, at a route such as `/notes` or `/item/<id>`. */
export async function openDashboard(route = "", flash = false) {
  try {
    await open(await pageUrl(route, flash));
  } catch (error) {
    await showZoneFailure(error, flash ? "Could Not Open Flash" : "Could Not Open the Page");
  }
}

/** Marks notes done or not done, with a toast. Returns whether it worked. */
export async function markDone(items: ZoneItem[], done: boolean): Promise<boolean> {
  const toast = await showToast({ style: Toast.Style.Animated, title: done ? "Marking done" : "Marking not done" });
  try {
    const result = await setDone(
      items.map((item) => item.id),
      done,
    );
    if (result.not_found.length > 0) {
      toast.style = Toast.Style.Failure;
      toast.title = "Not in the library anymore";
      return false;
    }
    toast.style = Toast.Style.Success;
    toast.title = done ? "Done" : "Not Done";
    return true;
  } catch (error) {
    await toast.hide();
    await showZoneFailure(error, "Could Not Change Done");
    return false;
  }
}

export async function pasteOrCopy(text: string, how: "paste" | "copy", what: string) {
  if (how === "paste") {
    await Clipboard.paste(text);
    await showHUD(`Pasted ${what}`);
  } else {
    await Clipboard.copy(text);
    await showHUD(`Copied ${what}`);
  }
}
