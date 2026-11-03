import { HostsPanel } from "./HostsPanel";

export function HostsScreen() {
  return (
    <div className="relative h-full overflow-hidden bg-background">
      <div className="h-full overflow-y-auto px-7 py-5">
        <HostsPanel />
      </div>
    </div>
  );
}
