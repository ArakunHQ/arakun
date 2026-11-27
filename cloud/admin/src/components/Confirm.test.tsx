// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { Confirm } from "./Confirm";

// jsdom has <dialog> but not its modal methods.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
    this.open = false;
  };
});

afterEach(cleanup);

function renderConfirm(onConfirm = vi.fn()) {
  render(
    <Confirm title="Decline octo?" body="They can ask again in 30 days." confirmLabel="Decline" onConfirm={onConfirm}>
      Decline
    </Confirm>,
  );
  return onConfirm;
}

describe("Confirm", () => {
  it("opens on its button with the focus on Cancel", async () => {
    renderConfirm();
    expect(screen.queryByRole("dialog")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Decline" }));
    expect(screen.getByRole("dialog", { name: "Decline octo?" })).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
  });

  it("calls onConfirm once on the confirm button and closes", async () => {
    const onConfirm = renderConfirm();
    await userEvent.click(screen.getByRole("button", { name: "Decline" }));
    const dialog = screen.getByRole("dialog");
    await userEvent.click(dialog.querySelector<HTMLButtonElement>("button:last-of-type")!);
    expect(onConfirm).toHaveBeenCalledOnce();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("calls nothing on Cancel", async () => {
    const onConfirm = renderConfirm();
    await userEvent.click(screen.getByRole("button", { name: "Decline" }));
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
