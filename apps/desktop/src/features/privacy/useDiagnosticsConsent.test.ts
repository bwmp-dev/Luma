import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useDiagnosticsConsent } from "./useDiagnosticsConsent";
import { invoke, setInvoke } from "../../test/tauriMock";

let root: Root;
let client: QueryClient;
let state: ReturnType<typeof useDiagnosticsConsent>;

function Probe() {
  state = useDiagnosticsConsent();
  return null;
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  root = createRoot(document.createElement("div"));
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  vi.unstubAllGlobals();
});

async function render() {
  await act(async () =>
    root.render(createElement(QueryClientProvider, { client }, createElement(Probe))),
  );
}

async function waitFor(assertion: () => void) {
  await vi.waitFor(async () => {
    await act(async () => {});
    assertion();
  });
}

describe("useDiagnosticsConsent", () => {
  it("only reads consent on mount, then sends explicit choices to the diagnostics command", async () => {
    let enabled = false;
    setInvoke((cmd, args) => {
      if (cmd === "diagnostics_config") return { configured: true, enabled };
      if (cmd === "diagnostics_set_enabled") {
        enabled = args.enabled === true;
        return;
      }
      throw new Error(`Unexpected command: ${cmd}`);
    });

    await render();
    await waitFor(() => expect(state.config.data).toEqual({ configured: true, enabled: false }));
    expect(invoke.mock.calls).toEqual([["diagnostics_config"]]);

    for (const choice of [true, false]) {
      await act(async () => state.choose.mutateAsync(choice));
      await waitFor(() => expect(state.config.data?.enabled).toBe(choice));
      expect(invoke).toHaveBeenCalledWith("diagnostics_set_enabled", { enabled: choice });
    }
  });

  it("preserves native consent when enabling fails", async () => {
    setInvoke((cmd) => {
      if (cmd === "diagnostics_config") return { configured: true, enabled: false };
      throw new Error("Could not save choice");
    });
    await render();
    await waitFor(() => expect(state.config.data?.enabled).toBe(false));

    await act(async () => {
      await expect(state.choose.mutateAsync(true)).rejects.toThrow("Could not save choice");
    });
    await waitFor(() => expect(state.choose.isError).toBe(true));
    expect(state.config.data?.enabled).toBe(false);
  });
});
