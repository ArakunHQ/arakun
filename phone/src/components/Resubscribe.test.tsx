import { render } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as push from "../lib/push";
import { LinkContext } from "../lib/link";
import { fakeLink } from "../test/screen";
import { Resubscribe } from "./Resubscribe";

beforeEach(() => {
  vi.stubGlobal("isSecureContext", true);
  vi.stubGlobal("PushManager", class {});
  vi.stubGlobal("Notification", { permission: "granted" });
  vi.spyOn(push, "subscribePush").mockResolvedValue();
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const mount = (me: object) =>
  render(
    <StrictMode>
      <LinkContext.Provider value={fakeLink({ me, pushKey: "KEY" } as never)}>
        <Resubscribe />
      </LinkContext.Provider>
    </StrictMode>,
  );

describe("Resubscribe", () => {
  it("subscribes once when allowed here but unknown to the computer", () => {
    mount({ notifying: false });
    expect(push.subscribePush).toHaveBeenCalledTimes(1);
    expect(push.subscribePush).toHaveBeenCalledWith("KEY");
  });

  it("does nothing when the computer already notifies this phone", () => {
    mount({ notifying: true });
    expect(push.subscribePush).not.toHaveBeenCalled();
  });

  it("does nothing without the permission", () => {
    vi.stubGlobal("Notification", { permission: "default" });
    mount({ notifying: false });
    expect(push.subscribePush).not.toHaveBeenCalled();
  });
});
