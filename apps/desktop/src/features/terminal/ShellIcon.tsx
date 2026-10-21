import { SquareTerminal } from "lucide-react";
import { DistroIcon, wslDistroOsId } from "../../components/DistroIcon";
import type { DetectedShell } from "../../lib/terminal";

export function ShellIcon({
  shell,
  size,
  className,
}: {
  shell: DetectedShell;
  size: number;
  className?: string;
}) {
  if (shell.kind === "wsl") {
    return (
      <DistroIcon
        osId={wslDistroOsId(shell.name)}
        size={size}
        className={className}
        label={`WSL ${shell.name}`}
      />
    );
  }
  return <SquareTerminal size={size} className={className} />;
}
