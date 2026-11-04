import { useId, useState } from "react";
import { Fingerprint, Loader2 } from "lucide-react";
import { parseLumaError } from "../../lib/hosts";
import { syncReadCertFingerprint } from "../../lib/sync";

/** Trust for a WebDAV server whose certificate no public CA signed, such as one
 * on a LAN address with a self-signed certificate. */
export function CertificateFingerprintField({
  url,
  value,
  onChange,
}: {
  url: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const inputId = useId();
  const noteId = useId();
  const [reading, setReading] = useState(false);
  const [readFromServer, setReadFromServer] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canRead = url.trim().toLowerCase().startsWith("https://") && !reading;

  const read = async () => {
    setReading(true);
    setError(null);
    try {
      onChange(await syncReadCertFingerprint(url.trim()));
      setReadFromServer(true);
    } catch (err) {
      setError(parseLumaError(err).message);
    } finally {
      setReading(false);
    }
  };

  return (
    <div>
      <div className="mb-1.5 flex items-baseline justify-between gap-2">
        <label htmlFor={inputId} className="text-sm font-medium">
          Certificate fingerprint
        </label>
        <span className="text-xs text-muted">Only for a self-signed certificate.</span>
      </div>
      <div className="flex gap-2">
        <input
          id={inputId}
          value={value}
          onChange={(e) => {
            onChange(e.target.value);
            setReadFromServer(false);
          }}
          placeholder="SHA-256, e.g. 3A:7F:…"
          spellCheck={false}
          autoComplete="off"
          aria-describedby={noteId}
          className="min-w-0 flex-1 rounded-md border border-border bg-background px-2.5 py-1.5 font-mono text-xs text-foreground outline-none placeholder:text-muted/80 focus:border-accent"
        />
        <button
          type="button"
          onClick={() => void read()}
          disabled={!canRead}
          className="flex shrink-0 items-center gap-1.5 rounded-md border border-border bg-raised px-3 py-1.5 text-sm font-medium text-foreground hover:border-accent/60 hover:bg-surface disabled:cursor-not-allowed disabled:opacity-50"
        >
          {reading ? (
            <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
          ) : (
            <Fingerprint size={14} />
          )}
          Read from server
        </button>
      </div>
      <p
        id={noteId}
        role={error ? "alert" : undefined}
        className={error ? "mt-1.5 text-xs text-danger" : "mt-1.5 text-xs text-muted"}
      >
        {error ??
          (readFromServer
            ? "Check that this matches the fingerprint your server shows before saving."
            : "Luma will trust only the certificate with this fingerprint.")}
      </p>
    </div>
  );
}
