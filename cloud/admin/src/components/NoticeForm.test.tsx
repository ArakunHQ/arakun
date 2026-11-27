// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { stubDialog } from "../test-utils";
import { NoticeForm } from "./NoticeForm";

const posts: unknown[] = [];

beforeAll(stubDialog);
beforeEach(() => {
  posts.length = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (_input, init) => {
      posts.push(JSON.parse(String(init?.body)));
      return new Response(JSON.stringify({ id: "n1" }), { status: 201 });
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function show(accountId?: number) {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <NoticeForm accountId={accountId} />
    </QueryClientProvider>,
  );
}

const send = () => screen.getByRole("button", { name: "Send notice" }) as HTMLButtonElement;

describe("NoticeForm", () => {
  it("sends the expiry as the end of the chosen day, local time", async () => {
    show(1002);
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Hi");
    await userEvent.type(screen.getByLabelText("Ends on (optional)"), "2030-03-05");
    await userEvent.click(send());
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({ account_id: 1002, text: "Hi", expires_at: new Date(2030, 2, 5, 23, 59, 59).getTime() / 1000 });
  });

  it("checks the link and the text before calling", async () => {
    show(1002);
    expect(send().disabled).toBe(true);
    const text = screen.getByRole("textbox", { name: "Text" });
    await userEvent.type(text, "Hi");
    await userEvent.type(screen.getByRole("textbox", { name: "Link (optional)" }), "http://example.com");
    expect(send().disabled).toBe(true);
    expect(screen.getByText("The link must be an https:// address.")).toBeTruthy();
    await userEvent.clear(screen.getByRole("textbox", { name: "Link (optional)" }));
    expect(send().disabled).toBe(false);
    await userEvent.click(text);
    await userEvent.paste("x".repeat(500));
    expect(send().disabled).toBe(true);
    expect(posts).toEqual([]);
  });

  it("asks before it goes to everyone", async () => {
    show();
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Maintenance tonight");
    await userEvent.click(send());
    const dialog = within(screen.getByRole("dialog", { name: "Send this notice to everyone?" }));
    await userEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(posts).toEqual([]);
    await userEvent.click(send());
    await userEvent.click(dialog.getByRole("button", { name: "Send to everyone" }));
    await waitFor(() => expect(posts).toEqual([{ text: "Maintenance tonight" }]));
  });

  it("sends to a typed GitHub ID without asking, and refuses a bad one", async () => {
    show();
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Hello");
    await userEvent.click(screen.getByRole("radio", { name: "One account" }));
    expect(send().disabled).toBe(true);
    const id = screen.getByRole("textbox", { name: "Account ID" });
    await userEvent.type(id, "10x");
    expect(send().disabled).toBe(true);
    await userEvent.clear(id);
    await userEvent.type(id, "1005");
    await userEvent.click(send());
    await waitFor(() => expect(posts).toEqual([{ account_id: 1005, text: "Hello" }]));
  });

  it("refuses a link with no host and a day that has passed", async () => {
    show(1002);
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Hi");
    const link = screen.getByRole("textbox", { name: "Link (optional)" });
    await userEvent.type(link, "https://");
    expect(send().disabled).toBe(true);
    await userEvent.clear(link);
    await userEvent.type(screen.getByLabelText("Ends on (optional)"), "2001-01-01");
    expect(send().disabled).toBe(true);
    expect(screen.getByText("Choose a day that has not passed.")).toBeTruthy();
    expect(posts).toEqual([]);
  });
});
