import { getTableName, type Table } from "drizzle-orm";
import { describe, expect, it, vi } from "vitest";
import { Database } from "../src/database.js";
import { AccountDeleter } from "../src/deletion.js";
import type { RoomRouter } from "../src/roomRouter.js";
import type { SnapshotStorage } from "../src/storage.js";

const ownerSubject = "owner";
const firstRoom = "11111111-1111-4111-8111-111111111111";
const secondRoom = "22222222-2222-4222-8222-222222222222";

interface QueryChain<T> extends PromiseLike<T> {
  from(...args: unknown[]): QueryChain<T>;
  where(...args: unknown[]): QueryChain<T>;
  returning(...args: unknown[]): QueryChain<T>;
}

function queryChain<T>(result: T): QueryChain<T> {
  const promise = Promise.resolve(result);
  const query = {} as QueryChain<T>;
  query.from = () => query;
  query.where = () => query;
  query.returning = () => query;
  query.then = promise.then.bind(promise);
  return query;
}

function databaseWithTransaction(transaction: object): Database {
  const database = Object.create(Database.prototype) as Database;
  Object.defineProperty(database, "orm", {
    value: {
      transaction: async (callback: (tx: object) => Promise<unknown>) =>
        await callback(transaction),
    } as unknown as Database["orm"],
  });
  return database;
}

/**
 * A transaction stub that hands each `select` its next queued result and
 * records every `delete` target, so a test can assert both what was removed
 * and in what order.
 */
function transactionSpy(selects: unknown[][]) {
  const deletes: string[] = [];
  const queue = [...selects];
  const transaction = {
    select: vi.fn(() => queryChain(queue.shift() ?? [])),
    delete: vi.fn((table: Table) => {
      deletes.push(getTableName(table));
      return queryChain([]);
    }),
  };
  return { transaction, deletes };
}

describe("collaboration account deletion", () => {
  it("removes sealed keys before the devices they reference", async () => {
    // `room_member_keys.device_id` references `devices` with no cascade, so
    // deleting a device first would violate the constraint.
    const { transaction, deletes } = transactionSpy([
      [{ id: firstRoom }],
      [{ id: "device-1" }],
    ]);
    const database = databaseWithTransaction(transaction);

    await database.deleteAccount(ownerSubject);

    const keys = deletes.indexOf("collaboration_room_member_keys");
    const devices = deletes.lastIndexOf("collaboration_devices");
    expect(keys).toBeGreaterThanOrEqual(0);
    expect(devices).toBeGreaterThan(keys);
  });

  it("deletes the account row last", async () => {
    const { transaction, deletes } = transactionSpy([
      [{ id: firstRoom }],
      [{ id: "device-1" }],
    ]);
    const database = databaseWithTransaction(transaction);

    await database.deleteAccount(ownerSubject);

    expect(deletes.at(-1)).toBe("collaboration_accounts");
  });

  it("reports what it removed", async () => {
    const { transaction } = transactionSpy([
      [{ id: firstRoom }, { id: secondRoom }],
      [{ id: "device-1" }],
    ]);
    const database = databaseWithTransaction(transaction);

    const report = await database.deleteAccount(ownerSubject);

    expect(report).toEqual({
      roomsDeleted: 2,
      membershipsRemoved: 0,
      devicesRemoved: 1,
    });
  });

  it("touches no rooms when the subject owns none", async () => {
    const { transaction, deletes } = transactionSpy([[], []]);
    const database = databaseWithTransaction(transaction);

    const report = await database.deleteAccount(ownerSubject);

    expect(report.roomsDeleted).toBe(0);
    expect(deletes).not.toContain("collaboration_rooms");
    expect(deletes).toContain("collaboration_accounts");
  });

  it("purges each owned room's snapshot and live state before its rows", async () => {
    const order: string[] = [];
    const database = {
      ownedRoomIds: vi.fn(async () => [firstRoom, secondRoom]),
      deleteAccount: vi.fn(async () => {
        order.push("database");
        return { roomsDeleted: 2, membershipsRemoved: 0, devicesRemoved: 0 };
      }),
    } as unknown as Database;
    const snapshots = {
      delete: vi.fn(async (roomId: string) => {
        order.push(`snapshot:${roomId}`);
      }),
    } as unknown as SnapshotStorage;
    const roomRouter = {
      evictRoom: vi.fn(async (roomId: string) => {
        order.push(`evict:${roomId}`);
      }),
    } as unknown as RoomRouter;

    const report = await new AccountDeleter(database, snapshots, roomRouter).purge(
      ownerSubject,
    );

    expect(report.roomsDeleted).toBe(2);
    expect(order).toEqual([
      `evict:${firstRoom}`,
      `snapshot:${firstRoom}`,
      `evict:${secondRoom}`,
      `snapshot:${secondRoom}`,
      "database",
    ]);
  });

  it("leaves rooms owned by other accounts alone", async () => {
    const database = {
      ownedRoomIds: vi.fn(async () => []),
      deleteAccount: vi.fn(async () => ({
        roomsDeleted: 0,
        membershipsRemoved: 3,
        devicesRemoved: 1,
      })),
    } as unknown as Database;
    const snapshots = { delete: vi.fn() } as unknown as SnapshotStorage;
    const roomRouter = { evictRoom: vi.fn() } as unknown as RoomRouter;

    const report = await new AccountDeleter(database, snapshots, roomRouter).purge(
      "member-only",
    );

    expect(snapshots.delete).not.toHaveBeenCalled();
    expect(roomRouter.evictRoom).not.toHaveBeenCalled();
    expect(report.membershipsRemoved).toBe(3);
  });
});
