import { openUrl } from "@tauri-apps/plugin-opener";
import { hasPlatformModifier } from "../../lib/platform";
import { isMobilePlatform } from "../../stores/capabilityStore";

export function openTerminalLink(event: MouseEvent, uri: string): void {
  if (isMobilePlatform() || hasPlatformModifier(event)) void openUrl(uri);
}
