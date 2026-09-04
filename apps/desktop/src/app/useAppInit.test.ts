import { afterEach, describe, expect, it } from "vitest";
import { emitWindowEvent, getCurrentDeepLinks, setCurrentDeepLinks } from "../test/tauriMock";
import { useUiStore } from "../stores/uiStore";
import { startDeepLinkListener } from "./useAppInit";

const vaultLink = "luma://vault?name=Team&provider=local-folder&path=%2Fshared%2Fluma";
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

afterEach(() => {
  useUiStore.getState().closeVaultJoin();
});

describe("deep-link startup", () => {
  it("handles the URL that launched a fully closed app", async () => {
    setCurrentDeepLinks([vaultLink]);

    const stop = startDeepLinkListener();
    await tick();
    await tick();

    expect(getCurrentDeepLinks).toHaveBeenCalledOnce();
    expect(useUiStore.getState().vaultJoinOpen).toBe(true);
    expect(useUiStore.getState().vaultJoinLink?.name).toBe("Team");
    stop();
  });

  it("continues to handle links delivered while running", async () => {
    const stop = startDeepLinkListener();
    await tick();
    emitWindowEvent("deep-link", vaultLink);

    expect(useUiStore.getState().vaultJoinOpen).toBe(true);
    stop();
  });
});
