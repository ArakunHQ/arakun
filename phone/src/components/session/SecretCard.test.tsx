import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { NOT_PRIVATE, type SecretRequest, SecretCard } from "./SecretCard";

const secret = (over: Partial<SecretRequest> = {}): SecretRequest => ({
  id: 4,
  label: "GitHub login",
  reason: "To push the branch.",
  url: "https://github.com/settings/tokens",
  fields: [
    { name: "user", label: "User name" },
    { name: "password", label: "Password" },
  ],
  private: true,
  ...over,
});

const mount = (r: SecretRequest) => {
  const onSave = vi.fn();
  const onDecline = vi.fn();
  render(<SecretCard secret={r} onSave={onSave} onDecline={onDecline} />);
  return { onSave, onDecline };
};

describe("SecretCard", () => {
  it("saves once every field is filled, masked until shown", () => {
    const { onSave } = mount(secret());
    const user = screen.getByLabelText("User name") as HTMLInputElement;
    const password = screen.getByLabelText("Password") as HTMLInputElement;
    expect(password.type).toBe("password");
    const save = screen.getByText("Save") as HTMLButtonElement;
    fireEvent.change(user, { target: { value: "octocat" } });
    expect(save.disabled).toBe(true);
    fireEvent.change(password, { target: { value: "hunter2" } });
    fireEvent.click(screen.getByLabelText("Show Password"));
    expect(password.type).toBe("text");
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledWith(["octocat", "hunter2"]);
    expect(password.value).toBe("");
  });

  it("offers only Decline over a connection that is not encrypted", () => {
    const { onDecline, onSave } = mount(secret({ private: false }));
    expect(screen.getByRole("alert").textContent).toBe(NOT_PRIVATE);
    expect(screen.queryByText("Save")).toBeNull();
    expect(screen.queryByLabelText("Password")).toBeNull();
    fireEvent.click(screen.getByText("Decline"));
    expect(onDecline).toHaveBeenCalled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("links only to web pages", () => {
    mount(secret({ url: "javascript:alert(1)" }));
    expect(screen.queryByText("javascript:alert(1)")).toBeNull();
  });
});
