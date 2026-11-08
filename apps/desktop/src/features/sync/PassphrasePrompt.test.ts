import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PassphrasePrompt } from "./PassphrasePrompt";
import { useCapabilityStore, DESKTOP_CAPABILITIES } from "../../stores/capabilityStore";
import { invoke, setInvoke } from "../../test/tauriMock";

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const onSubmit = vi.fn();

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  onSubmit.mockReset();
  useCapabilityStore
    .getState()
    .setCapabilities({ ...DESKTOP_CAPABILITIES, os: "android", isMobile: true });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  client.clear();
  useCapabilityStore.getState().setCapabilities(DESKTOP_CAPABILITIES);
  vi.unstubAllGlobals();
});

async function render(remember = true) {
  await act(async () =>
    root.render(
      createElement(
        QueryClientProvider,
        { client },
        createElement(PassphrasePrompt, {
          open: true,
          title: "Sync passphrase",
          onOpenChange: vi.fn(),
          rememberOption: true,
          rememberDefault: remember,
          onSubmit,
        }),
      ),
    ),
  );
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
}

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function button(label: string) {
  return Array.from(document.querySelectorAll("button")).find((b) => b.textContent === label)!;
}

describe("remembering a sync passphrase", () => {
  it.each([false, true])(
    "sets up or unlocks the mobile keystore before saving (configured=%s)",
    async (configured) => {
      let unlocked = false;
      setInvoke((cmd) => {
        if (cmd === "keystore_status") return { configured, unlocked, rememberOnDevice: false };
        if (cmd === "keystore_setup" || cmd === "keystore_unlock") {
          unlocked = true;
          return;
        }
        throw new Error(`Unexpected command: ${cmd}`);
      });
      await render();
      await type(
        document.querySelectorAll<HTMLInputElement>('input[type="password"]')[0],
        "sync secret",
      );
      expect(button("Continue").disabled).toBe(true);
      expect(onSubmit).not.toHaveBeenCalled();
      await type(
        document.querySelectorAll<HTMLInputElement>('input[type="password"]')[1],
        "master secret",
      );
      await act(async () => button(configured ? "Unlock keystore" : "Create keystore").click());
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
      expect(button("Continue").disabled).toBe(false);
      await act(async () => button("Continue").click());
      expect(onSubmit).toHaveBeenCalledWith("sync secret", true);
      expect(invoke).toHaveBeenCalledWith(
        configured ? "keystore_unlock" : "keystore_setup",
        configured
          ? { password: "master secret" }
          : { input: { password: "master secret", rememberDevice: false } },
      );
    },
  );

  it("allows a session-only mobile passphrase without a keystore", async () => {
    await render(false);
    await type(document.querySelector<HTMLInputElement>('input[type="password"]')!, "sync secret");
    await act(async () => button("Continue").click());
    expect(onSubmit).toHaveBeenCalledWith("sync secret", false);
    expect(invoke.mock.calls.some(([cmd]) => cmd === "keystore_status")).toBe(false);
  });

  it("keeps desktop remembering on the OS keychain", async () => {
    useCapabilityStore.getState().setCapabilities(DESKTOP_CAPABILITIES);
    await render();
    await type(document.querySelector<HTMLInputElement>('input[type="password"]')!, "sync secret");
    await act(async () => button("Continue").click());
    expect(onSubmit).toHaveBeenCalledWith("sync secret", true);
    expect(document.body.textContent).toContain("OS keychain");
    expect(invoke.mock.calls.some(([cmd]) => cmd === "keystore_status")).toBe(false);
  });
});
