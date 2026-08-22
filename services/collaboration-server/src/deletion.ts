import type { AccountDeletionReport, Database } from "./database.js";
import type { RoomRouter } from "./roomRouter.js";
import type { SnapshotStorage } from "./storage.js";

/**
 * Erases an account across Postgres, R2 and Redis.
 *
 * The database rows are the only index into the other two stores, so they are
 * deleted last: a failure partway through leaves a room whose snapshot and
 * live state are already gone but whose row still points at them, which a
 * retry finishes cleanly. Deleting the rows first would strand an R2 object
 * that nothing can find and nothing can remove.
 */
export class AccountDeleter {
  constructor(
    private readonly database: Database,
    private readonly snapshots: SnapshotStorage,
    private readonly roomRouter: RoomRouter,
  ) {}

  /** Remove every trace of `subject`. Idempotent. */
  async purge(subject: string): Promise<AccountDeletionReport> {
    for (const roomId of await this.database.ownedRoomIds(subject)) {
      await this.roomRouter.evictRoom(roomId);
      await this.snapshots.delete(roomId);
    }
    return await this.database.deleteAccount(subject);
  }
}
