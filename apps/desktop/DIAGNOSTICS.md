# Desktop opt-out diagnostics

Luma uses the self-hosted Aptabase fork's native JSON endpoints:
POST /api/v0/error and POST /api/v0/transaction, authenticated with the existing
Luma App-Key from LUMA_ANALYTICS_HOST / LUMA_ANALYTICS_KEY. No Sentry DSN,
owner cookie, new credential, SDK dependency or symbol upload is needed.
An invalid/off analytics build configuration disables both transports.
The runtime product analytics and diagnostics preferences are independent.

## Consent and data

Diagnostics defaults ON on desktop when no preference is saved, for both fresh
installs and upgrades, regardless of the usage analytics preference. A saved
false or true is always respected; malformed saved values fail closed. Mobile
defaults OFF. Turn it off in desktop Settings > Privacy > Share diagnostics. The backend persists privacy.diagnostics only on this device;
sync excludes the preference. Generic settings writes/deletion use the same
runtime consent path. Disabling clears queued reports and waits for the
currently in-flight request to finish (up to the three-second HTTP timeout).
It cannot recall a request already accepted by the server.

Diagnostics stays inactive until settings are loaded successfully. Early-start
failures and settings-read failures are not reported, and an unavailable runtime
handle or invalid build configuration stays disabled. No preference migration
or write occurs merely to apply the default. UI loading/error states never
assume permission to send; the backend is authoritative. Deleting the preference
through the settings command persists false rather than re-enabling the default.

Product analytics retains launches, duration and its existing random install
ID. Failure/panic reporting has moved out of that path: the product analytics
switch alone no longer enables diagnostics.

Diagnostics emits only:

- An allowlisted failure category when a Rust command error reaches the frontend.
  It never serializes the original error message. Repeats are limited to the
  first, second, fourth through 128th occurrence per category/process.
- A Rust panic's source basename and line, with a fixed category. No panic
  payload, source contents or full source path is sent.
- Successful backend setup duration, rounded down to 10 milliseconds, on
  approximately 10% of launches where diagnostics was already enabled.
- App release, platform, fixed development/production label and isDebug flag.
  The same App-Key serves both modes; isDebug separates collector storage.

There are no install, account or session identifiers in these payloads, nor
hostnames, connection targets, commands, terminal output, keys, credentials,
breadcrumbs, free-form messages or runtime file paths. Event UUIDs identify
individual reports only and are retained unchanged for retries.

## Delivery and limits

An in-memory queue holds at most 32 reports for at most ten minutes. A worker
sends at most four reports every five seconds. Each HTTP request times out at
three seconds; redirects are disabled so the key cannot follow redirects.
Transport errors, 429 and 5xx receive at most three attempts with bounded
backoff; other HTTP failures are dropped. A server Retry-After delta-seconds
value is respected up to ten minutes. No network error body is logged.
Queue overflow drops the oldest item. There is no persistent offline queue:
exit, process termination or prolonged downtime may lose reports.

This is sampled diagnostics, not exact fleet failure counts. Rust panic
reporting is best effort and may be lost when a process terminates; it is not
native crash capture. No sessions, minidumps, native signal/Win32 exception
capture, source maps or server symbolication are added.

The new setting is currently exposed in desktop settings. Mobile starts with
diagnostics OFF and cannot enable it through the mobile UI yet. Existing
mobile analytics disclosure was corrected to usage-only. No mobile integration
or iOS Simulator verification is claimed.

## Verification

Tests cover safe payload construction, unknown-category suppression, Windows
and Unix path stripping, desktop/mobile defaults, saved preference persistence and queue limits, bounded retry counts,
expiry, and actual reqwest HTTP against synthetic loopback endpoints. The
loopback verifies headers/routes, identical UUID/body across 429 retry, 401
drop, and transaction routing. It is not end-to-end acceptance against a live
Aptabase/PostgreSQL collector, and sends no production telemetry.

Contract references:

- https://github.com/bwmp-dev/aptabase/blob/main/docs/error-api-openapi.yaml
- https://github.com/bwmp-dev/aptabase/blob/main/docs/app-performance-api.md
