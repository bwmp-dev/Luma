import { describe, expect, it } from "vitest";

import { buildVaultJoinLink, parseVaultJoinLink, type VaultJoinLink } from "./vaults";

const webdav: VaultJoinLink = {
  name: "Home lab",
  provider: "webdav",
  inviteSecret: null,
  folderPath: null,
  url: "https://192.168.1.20/dav/luma",
  username: "sample-user",
  certSha256: `${"AB:".repeat(31)}CD`,
  gistId: null,
  cloudUrl: null,
};

describe("vault join links", () => {
  it("carry a pinned WebDAV certificate so joiners trust the same server", () => {
    expect(parseVaultJoinLink(buildVaultJoinLink(webdav))).toEqual(webdav);
  });

  it("omit the certificate when the server has a publicly trusted one", () => {
    const link = buildVaultJoinLink({ ...webdav, certSha256: null });
    expect(new URL(link).searchParams.has("cert")).toBe(false);
    expect(parseVaultJoinLink(link)?.certSha256).toBeNull();
  });
});
