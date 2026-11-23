import { latestClip } from "../lib/agent-zone";

/**
 * Get the newest clip: text the person sent from Zone on their iPhone to this computer on purpose
 * (from the phone's clipboard, shared from an app, a shortcut, or typed). It is exact text, not
 * speech. Use it for "what I just sent", "the link from my phone" or "the thing I copied on my
 * phone".
 */
export default async function tool() {
  const clip = await latestClip();
  if (!clip) return { clip: null, note: "No clip has been sent from the phone yet." };
  return {
    clip: {
      id: clip.id,
      source: clip.source,
      time: clip.created_local,
      ageSeconds: clip.age_s,
      characters: clip.length,
      text: clip.text,
    },
  };
}
