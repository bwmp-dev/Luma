import { afterEach, describe, expect, it, vi } from "vitest";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCapabilityStore } from "../../stores/capabilityStore";
import { openTerminalLink } from "./terminalLinks";

afterEach(() => {
  vi.restoreAllMocks();
  useCapabilityStore.setState({
    capabilities: { ...useCapabilityStore.getState().capabilities, isMobile: false },
  });
});

describe("terminal link activation", () => {
  it.each([
    ["Macintosh", "metaKey", "ctrlKey"],
    ["Windows", "ctrlKey", "metaKey"],
    ["Linux", "ctrlKey", "metaKey"],
  ])("requires the platform modifier on %s", (platform, modifier, other) => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue(platform);
    vi.mocked(openUrl).mockClear();
    openTerminalLink(new MouseEvent("click"), "https://example.com");
    openTerminalLink(new MouseEvent("click", { [other]: true }), "https://example.com");
    expect(openUrl).not.toHaveBeenCalled();
    openTerminalLink(new MouseEvent("click", { [modifier]: true }), "https://example.com");
    expect(openUrl).toHaveBeenCalledExactlyOnceWith("https://example.com");
  });
  it("preserves touch activation on mobile", () => {
    useCapabilityStore.setState({
      capabilities: { ...useCapabilityStore.getState().capabilities, isMobile: true },
    });
    vi.mocked(openUrl).mockClear();
    openTerminalLink(new MouseEvent("click"), "https://example.com");
    expect(openUrl).toHaveBeenCalledExactlyOnceWith("https://example.com");
  });
});
