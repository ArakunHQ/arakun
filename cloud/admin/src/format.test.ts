import { describe, expect, it } from "vitest";
import { age, githubUrl, parseGithubId, relative } from "./format";

const DAY = 24 * 60 * 60;
const now = 1_800_000_000;

describe("age", () => {
  it("is under a minute at 0", () => {
    expect(age(now, now)).toBe("under a minute");
  });

  it("counts days up to 59", () => {
    expect(age(now - 59 * DAY, now)).toBe("59 days");
    expect(age(now - DAY, now)).toBe("1 day");
  });

  it("counts months at 13 months", () => {
    expect(age(now - 13 * 30 * DAY, now)).toBe("13 months");
  });

  it("counts years at 3 years", () => {
    expect(age(now - 3 * 365 * DAY, now)).toBe("3 years");
  });

  it("counts minutes and hours below a day", () => {
    expect(age(now - 5 * 60, now)).toBe("5 minutes");
    expect(age(now - 3 * 60 * 60, now)).toBe("3 hours");
  });
});

describe("relative", () => {
  it("says ago for the past and in for the future", () => {
    expect(relative(now - 2 * DAY, now)).toBe("2 days ago");
    expect(relative(now + 2 * DAY, now)).toBe("in 2 days");
  });

  it("says just now within a minute", () => {
    expect(relative(now - 30, now)).toBe("just now");
    expect(relative(now + 30, now)).toBe("just now");
  });
});

describe("githubUrl", () => {
  it("links a valid login", () => {
    expect(githubUrl("octo-cat_1")).toBe("https://github.com/octo-cat_1");
  });

  it("gives null for an empty login or one with other characters", () => {
    expect(githubUrl("")).toBeNull();
    expect(githubUrl("a/b")).toBeNull();
    expect(githubUrl("a b")).toBeNull();
    expect(githubUrl("../x")).toBeNull();
    expect(githubUrl("a".repeat(40))).toBeNull();
  });
});

describe("parseGithubId", () => {
  it("accepts the IDs the Worker accepts and nothing else", () => {
    expect(parseGithubId("1002")).toBe(1002);
    for (const bad of ["", "0", "012", "-1", "1e3", "12a", " 1", "99999999999999999", "9007199254740993"]) expect(parseGithubId(bad)).toBeNull();
  });
});
