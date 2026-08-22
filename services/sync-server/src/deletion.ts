import { deleteAccountRow } from "./accounts";
import { accountTarget, deleteAll } from "./sync";
import type { Account, Env, Vault } from "./types";

/**
 * Erasing an account is a privacy promise, so it reports what it removed rather
 * than succeeding silently: the client shows the counts and the tests assert on
 * them.
 */
export type AccountDeletionReport = {
  vaultsDeleted: number;
  membershipsRemoved: number;
  devicesRemoved: number;
};

/** D1 caps bound parameters per statement, so owned-vault ids go in chunks. */
const IDS_PER_STATEMENT = 90;

/**
 * Remove every trace of `subject` from this deployment.
 *
 * Blobs go before rows. The rows are the only index into R2, so losing them
 * first would strand objects that nothing can find and nothing can delete —
 * the opposite of what deleting an account is supposed to achieve. Failing
 * midway instead leaves rows pointing at blobs that are already gone, which a
 * retry finishes cleanly.
 *
 * Idempotent: a second call finds nothing and reports zeroes.
 */
export async function purgeAccountData(
  env: Env,
  account: Account,
): Promise<AccountDeletionReport> {
  const owned = await ownedVaults(env, account.subject);

  // The blob prefix is built here rather than via `vaultTarget`, which also
  // resolves the owner's quota and would fail on an account mid-deletion.
  // Deleting only needs to know where the bytes are.
  for (const vault of owned) {
    await deleteAll(env, { prefix: `vaults/${vault.storage_id}` });
  }
  await deleteAll(env, accountTarget(account));

  const membershipsRemoved = await countRows(
    env,
    `SELECT COUNT(*) AS count FROM vault_members WHERE subject = ?1`,
    account.subject,
  );
  const devicesRemoved = await countRows(
    env,
    `SELECT COUNT(*) AS count FROM vault_devices WHERE subject = ?1`,
    account.subject,
  );

  const ownedIds = owned.map((vault) => vault.id);
  for (const ids of chunk(ownedIds, IDS_PER_STATEMENT)) {
    const placeholders = ids.map((_, index) => `?${index + 1}`).join(", ");
    for (const table of ["vault_member_keys", "vault_invites", "vault_members"]) {
      await run(env, `DELETE FROM ${table} WHERE vault_id IN (${placeholders})`, ids);
    }
  }

  // Their own rows in vaults other people own: the membership and the keys
  // sealed to their devices. Those vaults themselves are untouched.
  await run(env, `DELETE FROM vault_member_keys WHERE subject = ?1`, [account.subject]);
  await run(env, `DELETE FROM vault_members WHERE subject = ?1`, [account.subject]);
  await run(
    env,
    `DELETE FROM vault_invites WHERE created_by_subject = ?1`,
    [account.subject],
  );
  await run(env, `DELETE FROM vaults WHERE owner_subject = ?1`, [account.subject]);
  await run(env, `DELETE FROM vault_devices WHERE subject = ?1`, [account.subject]);
  await deleteAccountRow(env, account.subject);

  return {
    vaultsDeleted: owned.length,
    membershipsRemoved,
    devicesRemoved,
  };
}

/**
 * Every vault this subject owns, including any already soft-deleted — a
 * tombstoned vault still has a blob in R2 and rows in D1, and this is the last
 * chance to reclaim them.
 */
async function ownedVaults(env: Env, subject: string): Promise<Vault[]> {
  const result = await env.DB.prepare(
    `SELECT id, owner_subject, storage_id, key_epoch, used_bytes, deleted_at
     FROM vaults WHERE owner_subject = ?1`,
  )
    .bind(subject)
    .all<Vault>();
  return result.results ?? [];
}

async function countRows(env: Env, query: string, subject: string): Promise<number> {
  const row = await env.DB.prepare(query).bind(subject).first<{ count: number }>();
  return row?.count ?? 0;
}

async function run(env: Env, query: string, values: unknown[]): Promise<void> {
  await env.DB.prepare(query)
    .bind(...values)
    .run();
}

function chunk<T>(items: T[], size: number): T[][] {
  const chunks: T[][] = [];
  for (let index = 0; index < items.length; index += size) {
    chunks.push(items.slice(index, index + size));
  }
  return chunks;
}
