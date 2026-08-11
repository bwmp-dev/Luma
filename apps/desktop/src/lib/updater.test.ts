import { describe, expect, it, vi } from "vitest";
import { setInvoke } from "../test/tauriMock";
import { checkForUpdate } from "./updater";

vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn() }));

describe("updater channel bridge", () => {
  it("checks the selected channel and forwards native download progress", async () => {
    const events: unknown[] = [];
    setInvoke((command, args) => {
      if (command === "updater_check") {
        expect(args.channel).toBe("nightly");
        return {
          version: "0.17.0-nightly.42",
          currentVersion: "0.16.0",
          notes: "  Nightly changes  ",
        };
      }
      if (command === "updater_download_and_install") {
        const channel = args.onEvent as {
          onmessage: (event: unknown) => void;
        };
        channel.onmessage({ event: "Progress", data: { chunkLength: 512 } });
        return undefined;
      }
      throw new Error(`unexpected command: ${command}`);
    });

    const found = await checkForUpdate("nightly");

    expect(found?.info).toEqual({
      version: "0.17.0-nightly.42",
      currentVersion: "0.16.0",
      notes: "Nightly changes",
    });
    await found?.update.downloadAndInstall((event) => events.push(event));
    expect(events).toEqual([
      { event: "Progress", data: { chunkLength: 512 } },
    ]);
  });

  it("returns null when the selected channel is current", async () => {
    setInvoke((command, args) => {
      expect(command).toBe("updater_check");
      expect(args.channel).toBe("stable");
      return null;
    });

    await expect(checkForUpdate("stable")).resolves.toBeNull();
  });
});
