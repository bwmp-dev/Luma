import * as Dialog from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import { useMediaQuery } from "../hooks/useMediaQuery";

export function InspectorShell({
  title,
  subtitle,
  onClose,
  footer,
  children,
}: {
  title: string;
  subtitle: string;
  onClose: () => void;
  footer: React.ReactNode;
  children: React.ReactNode;
}) {
  const compact = useMediaQuery("(max-width: 767px)");
  const header = (
    <>
      <div className="min-w-0 flex-1">
        <h2 className="truncate text-sm font-semibold">{title}</h2>
        <p className="truncate text-xs text-muted">{subtitle}</p>
      </div>
    </>
  );
  if (compact) {
    return (
      <Dialog.Root
        open
        onOpenChange={(open) => {
          if (!open) onClose();
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-40 bg-black/50" />
          <Dialog.Content
            aria-describedby={undefined}
            className="fixed inset-x-0 bottom-0 z-50 flex max-h-[85dvh] flex-col rounded-t-2xl border-t border-border bg-surface pb-safe focus:outline-none"
          >
            <div className="flex items-center gap-3 border-b border-border px-4 py-2">
              <Dialog.Title asChild>
                <div className="min-w-0 flex-1">
                  <h2 className="truncate text-sm font-semibold">{title}</h2>
                  <p className="truncate text-xs text-muted">{subtitle}</p>
                </div>
              </Dialog.Title>
              <Dialog.Close className="-mr-2 flex min-h-11 shrink-0 items-center rounded-lg px-3 text-sm text-muted active:bg-raised">
                Cancel
              </Dialog.Close>
            </div>
            <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">{children}</div>
            <div className="border-t border-border p-4">{footer}</div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    );
  }
  return (
    <aside className="flex w-90 shrink-0 flex-col border-l border-border bg-surface">
      <header className="flex h-14 items-center border-b border-border px-4">
        {header}
        <button
          onClick={onClose}
          aria-label="Close"
          className="rounded p-1 text-muted hover:bg-raised"
        >
          <X size={16} />
        </button>
      </header>
      <div className="flex-1 space-y-4 overflow-y-auto p-4">{children}</div>
      <footer className="border-t border-border p-4">{footer}</footer>
    </aside>
  );
}
