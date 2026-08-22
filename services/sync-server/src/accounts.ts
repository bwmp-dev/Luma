import type { Account, Env } from "./types";
import { HttpError } from "./auth";

export async function getOrCreateAccount(env: Env, subject: string): Promise<Account> {
  const quota = positiveInteger(env.DEFAULT_QUOTA_BYTES, "DEFAULT_QUOTA_BYTES");
  const storageId = crypto.randomUUID();
  await env.DB.prepare(
    `INSERT OR IGNORE INTO accounts (subject, storage_id, quota_bytes)
     VALUES (?1, ?2, ?3)`,
  )
    .bind(subject, storageId, quota)
    .run();

  const account = await env.DB.prepare(
    `SELECT subject, storage_id, quota_bytes, used_bytes, deleted_at
     FROM accounts WHERE subject = ?1`,
  )
    .bind(subject)
    .first<Account>();
  if (!account || account.deleted_at !== null) {
    throw new HttpError(403, "account is unavailable");
  }
  return account;
}

export async function updateUsage(env: Env, subject: string, bytes: number): Promise<void> {
  await env.DB.prepare(
    `UPDATE accounts
     SET used_bytes = ?2, updated_at = unixepoch()
     WHERE subject = ?1 AND deleted_at IS NULL`,
  )
    .bind(subject, bytes)
    .run();
}

export function positiveInteger(value: string, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

/**
 * Remove the account row outright rather than tombstoning it.
 *
 * `getOrCreateAccount` rejects any subject whose `deleted_at` is set, and
 * nothing ever clears it, so a soft delete would permanently lock out a user
 * who deleted their Luma data but kept their identity provider account and
 * later signed in again. Identity provider subjects are not reused, so dropping
 * the row simply lets a returning subject start fresh. It also means the
 * subject itself — an account identifier we promise to erase — does not linger.
 */
export async function deleteAccountRow(env: Env, subject: string): Promise<void> {
  await env.DB.prepare(`DELETE FROM accounts WHERE subject = ?1`)
    .bind(subject)
    .run();
}
