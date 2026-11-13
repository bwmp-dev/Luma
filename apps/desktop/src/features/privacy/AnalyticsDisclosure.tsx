/*
 * The plain-language disclosure shared by the desktop dialog, the mobile sheet
 * and both Settings screens.
 */
export function AnalyticsDisclosure() {
  return (
    <div className="space-y-3 text-sm text-muted">
      <p>
        Luma can send a small record of app launches so we know which versions and platforms are
        still in use.
      </p>
      <div>
        <p className="font-medium text-foreground">What is sent</p>
        <ul className="mt-1 list-disc space-y-0.5 pl-5">
          <li>That the app was launched, and how long it stayed open</li>
          <li>The app version and your operating system</li>
          <li>Whether this is a development build</li>
          <li>A random ID so we can count repeat launches from the same install</li>
          <li>
            An approximate location (country, region and city) that our server looks up from your IP
            address when a report arrives. The address itself is not stored.
          </li>
        </ul>
      </div>
      <div>
        <p className="font-medium text-foreground">What is never sent with analytics</p>
        <ul className="mt-1 list-disc space-y-0.5 pl-5">
          <li>Hostnames, usernames, commands, output or file paths</li>
          <li>Keys, passwords or anything from a vault</li>
          <li>Your name, email or Luma account identity</li>
          <li>Failure categories, error messages or crash details</li>
        </ul>
      </div>
      <p>
        The install ID is never sent to your other devices and is deleted when you turn this off.
        Reports go to a server operated by Luma.
      </p>
    </div>
  );
}
