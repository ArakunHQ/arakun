import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Entry, Session } from "../../lib/model";
import { Transcript } from "./Transcript";

const session = (entries: Entry[], from = 0): Session => ({
  id: "a",
  title: "",
  agent: "claude",
  status: "working",
  count: from + entries.length,
  from,
  entries,
  permission: null,
  question: null,
  place: "",
});

const mount = (s: Session) => {
  const props = { loadingOlder: false, onLoadOlder: vi.fn(), pending: null, onHoldBubble: vi.fn() };
  const out = render(<Transcript session={s} {...props} />);
  const log = out.container.querySelector(".log") as HTMLDivElement;
  // jsdom lays nothing out: the list is 100 px high and its content 1000 px.
  Object.defineProperty(log, "clientHeight", { configurable: true, value: 100 });
  Object.defineProperty(log, "scrollHeight", { configurable: true, value: 1000 });
  return { ...out, log, again: (next: Session) => out.rerender(<Transcript session={next} {...props} />) };
};

const frame = () => act(() => new Promise((r) => requestAnimationFrame(() => r(undefined))));
const notice = (text: string): Entry => ({ type: "notice", text });

describe("Transcript", () => {
  it("follows new lines while at the end", () => {
    const first = [notice("a"), notice("b")];
    const { log, again } = mount(session(first));
    log.scrollTop = 880;
    fireEvent.scroll(log);
    again(session([...first, notice("c")]));
    expect(log.scrollTop).toBe(1000);
    expect(screen.queryByText(/new$/)).toBeNull();
  });

  it("counts what came while reading further up, and goes back down", () => {
    const first = [notice("a"), notice("b")];
    const { log, again } = mount(session(first));
    log.scrollTop = 0;
    fireEvent.scroll(log);
    again(session([...first, notice("c"), notice("d")]));
    expect(log.scrollTop).toBe(0);
    fireEvent.click(screen.getByText("2 new"));
    expect(log.scrollTop).toBe(1000);
    expect(screen.queryByText("2 new")).toBeNull();
  });

  it("keeps an opened tool call open after it scrolled away and back", async () => {
    const tools = Array.from({ length: 60 }, (_, i) => ({ type: "tool", title: `t${i}`, status: "completed", output: "out" }));
    const { log, container } = mount(session(tools));
    log.scrollTop = 0;
    fireEvent.scroll(log);
    await frame();
    const first = () => container.querySelector('[data-abs="0"] details') as HTMLDetailsElement | null;
    first()!.open = true;
    first()!.dispatchEvent(new Event("toggle"));
    log.scrollTop = 2000;
    fireEvent.scroll(log);
    await frame();
    expect(first()).toBeNull();
    log.scrollTop = 0;
    fireEvent.scroll(log);
    await frame();
    expect(first()!.open).toBe(true);
  });

  it("asks for older entries near the top", () => {
    const props = { loadingOlder: false, onLoadOlder: vi.fn(), pending: null, onHoldBubble: vi.fn() };
    render(<Transcript session={session([notice("a")], 50)} {...props} />);
    expect(props.onLoadOlder).toHaveBeenCalled();
  });
});
