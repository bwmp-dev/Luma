import { create } from "zustand";

/**
 * Which pane the "Share with agent" dialog is opened for.
 *
 * Selection state only, matching the other stores: the shared-pane list itself
 * is backend state and is read through react-query.
 */
type ShareTarget = {
  sessionId: string;
  title: string;
};

type McpShareState = {
  target: ShareTarget | null;
  open: (target: ShareTarget) => void;
  close: () => void;
};

export const useMcpShareStore = create<McpShareState>((set) => ({
  target: null,
  open: (target) => set({ target }),
  close: () => set({ target: null }),
}));
