import { describe, expect, it } from "vitest";
import { betaLine, canJoin, type Signup, thisPlatform } from "./beta";

const signup = (status: Signup["status"], retry_at: number | null = null): Signup => ({ status, email: "me@icloud.com", retry_at });

describe("beta", () => {
  it("knows the phone it runs on", () => {
    expect(thisPlatform("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", 5)).toBe("ios");
    expect(thisPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5)).toBe("ios");
    expect(thisPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0)).toBeNull();
    expect(thisPlatform("Mozilla/5.0 (Linux; Android 15; Pixel 9)", 5)).toBe("android");
  });

  it("lets an account sign up until it has, or after a decline has cooled down", () => {
    expect(canJoin(signup("none"))).toBe(true);
    expect(canJoin(signup("pending"))).toBe(false);
    expect(canJoin(signup("admitted"))).toBe(false);
    expect(canJoin(signup("declined", 200), 100)).toBe(false);
    expect(canJoin(signup("declined", 100), 100)).toBe(true);
  });

  it("says where each sign-up stands", () => {
    expect(betaLine("ios", signup("pending"))).toContain("TestFlight invite will go to me@icloud.com");
    expect(betaLine("android", signup("admitted"))).toContain("Google Play");
    expect(betaLine("ios", signup("declined", 100 + 2 * 86_400 - 5), 100)).toContain("2 days");
  });
});
