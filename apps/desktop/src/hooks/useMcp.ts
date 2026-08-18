import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createMcpGrant,
  deleteMcpGrant,
  getMcpExecutablePath,
  getMcpStatus,
  listMcpActivity,
  listMcpGrants,
  listSharedPanes,
  sharePane,
  unsharePane,
  updateMcpGrant,
} from "../lib/mcp";

export const MCP_KEY = ["mcp"];
const GRANTS_KEY = [...MCP_KEY, "grants"];
const STATUS_KEY = [...MCP_KEY, "status"];
const SHARED_PANES_KEY = [...MCP_KEY, "shared-panes"];
const ACTIVITY_KEY = [...MCP_KEY, "activity"];

export function useMcpGrants() {
  return useQuery({ queryKey: GRANTS_KEY, queryFn: listMcpGrants });
}

export function useMcpStatus() {
  return useQuery({ queryKey: STATUS_KEY, queryFn: getMcpStatus });
}

export function useSharedPanes() {
  return useQuery({ queryKey: SHARED_PANES_KEY, queryFn: listSharedPanes });
}

/** Recent agent activity. Polled while the section is open so a command an
 * agent runs elsewhere shows up without the user reloading anything. */
export function useMcpActivity(enabled: boolean) {
  return useQuery({
    queryKey: ACTIVITY_KEY,
    queryFn: listMcpActivity,
    refetchInterval: enabled ? 2_000 : false,
    enabled,
  });
}

/** Path the client should launch. Stable for the life of the process. */
export function useMcpExecutablePath() {
  return useQuery({
    queryKey: [...MCP_KEY, "executable"],
    queryFn: getMcpExecutablePath,
    staleTime: Infinity,
  });
}

export function useMcpGrantMutations() {
  const queryClient = useQueryClient();
  // Creating or deleting a grant also starts or stops the listener, so status
  // and shares are invalidated alongside the grant list.
  const invalidate = () => queryClient.invalidateQueries({ queryKey: MCP_KEY });

  const create = useMutation({
    mutationFn: ({
      label,
      hostIds,
      requireApproval,
    }: {
      label: string;
      hostIds: string[];
      requireApproval: boolean;
    }) => createMcpGrant(label, hostIds, requireApproval),
    onSuccess: invalidate,
  });

  const update = useMutation({
    mutationFn: ({
      id,
      label,
      hostIds,
      requireApproval,
    }: {
      id: string;
      label: string;
      hostIds: string[];
      requireApproval: boolean;
    }) => updateMcpGrant(id, label, hostIds, requireApproval),
    onSuccess: invalidate,
  });

  const remove = useMutation({
    mutationFn: (id: string) => deleteMcpGrant(id),
    onSuccess: invalidate,
  });

  return { create, update, remove };
}

export function usePaneShareMutations() {
  const queryClient = useQueryClient();
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: SHARED_PANES_KEY });

  const share = useMutation({
    mutationFn: ({
      sessionId,
      grantId,
      title,
    }: {
      sessionId: string;
      grantId: string;
      title: string;
    }) => sharePane(sessionId, grantId, title),
    onSuccess: invalidate,
  });

  const unshare = useMutation({
    mutationFn: (sessionId: string) => unsharePane(sessionId),
    onSuccess: invalidate,
  });

  return { share, unshare };
}
