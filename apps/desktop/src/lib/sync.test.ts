import { describe, it, expect } from "vitest";
import {
  AUTO_INTERVAL_CHOICES,
  formatCadence,
  pushScheduleValue,
  withPushSchedule,
  type AutoSyncSettings,
} from "./sync";

const BASE: AutoSyncSettings = {
  pushMode: "on-change",
  pushIntervalMinutes: 15,
  pullIntervalMinutes: 15,
  pullOnStart: true,
  pullOnFocus: true,
};

describe("automatic sync schedules", () => {
  it("renders a cadence in the largest unit that stays whole", () => {
    expect(formatCadence(5)).toBe("5 minutes");
    expect(formatCadence(30)).toBe("30 minutes");
    expect(formatCadence(60)).toBe("1 hour");
    expect(formatCadence(360)).toBe("6 hours");
    expect(formatCadence(1440)).toBe("1 day");
  });

  it("round-trips every dropdown value the picker can offer", () => {
    const values = ["off", "on-change", ...AUTO_INTERVAL_CHOICES.map(String)];
    for (const value of values) {
      expect(pushScheduleValue(withPushSchedule(BASE, value))).toBe(value);
    }
  });

  it("keeps the chosen cadence when switching the push mode off and back", () => {
    const hourly = withPushSchedule(BASE, "60");
    expect(hourly.pushMode).toBe("interval");
    expect(hourly.pushIntervalMinutes).toBe(60);

    // Switching away must not lose the number: the backend ignores it while the
    // mode is not "interval", so it is the only memory of the user's choice.
    const off = withPushSchedule(hourly, "off");
    expect(off.pushIntervalMinutes).toBe(60);
    expect(pushScheduleValue(off)).toBe("off");
  });

  it("changes only the push side", () => {
    const next = withPushSchedule(
      { ...BASE, pullIntervalMinutes: 30, pullOnFocus: false },
      "off",
    );
    expect(next.pullIntervalMinutes).toBe(30);
    expect(next.pullOnFocus).toBe(false);
    expect(next.pullOnStart).toBe(true);
  });
});
