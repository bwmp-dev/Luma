export function DiagnosticsDisclosure() {
  return (
    <div id="diagnostics-disclosure" className="space-y-2 text-sm text-muted">
      <p>
        On by default on desktop. You can turn this off here. While enabled, this device sends
        reports to Luma's reporting server. They include failure categories, the source filename and
        line for Rust panics, and rough startup times from a sample of launches, plus the app
        version, platform, and whether it is a development build.
      </p>
      <p>
        Rust panic reports are best effort and may be lost if the app exits. This does not capture
        native crashes.
      </p>
      <p>
        Reports contain no install, account, or session IDs; hostnames or connection addresses;
        commands or terminal output; your keys, passwords, or tokens; error text or full file paths.
        This choice stays on this device and is separate from analytics.
      </p>
    </div>
  );
}
