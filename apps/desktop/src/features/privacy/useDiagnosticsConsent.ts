import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

const DIAGNOSTICS_CONFIG_KEY = ["diagnostics-config"];

type DiagnosticsConfig = {
  configured: boolean;
  enabled: boolean;
};

export function useDiagnosticsConsent() {
  const queryClient = useQueryClient();
  const config = useQuery({
    queryKey: DIAGNOSTICS_CONFIG_KEY,
    queryFn: () => invoke<DiagnosticsConfig>("diagnostics_config"),
    staleTime: Infinity,
  });
  const choose = useMutation({
    // The backend owns both persistence and the live switch. Never derive
    // diagnostics consent from the separate analytics preference.
    mutationFn: (enabled: boolean) => invoke<void>("diagnostics_set_enabled", { enabled }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: DIAGNOSTICS_CONFIG_KEY }),
  });

  return { config, choose };
}
