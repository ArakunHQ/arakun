import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { type Question, QuestionCard } from "./QuestionCard";

const question: Question = {
  tool_call_id: "q1",
  questions: [
    { question: "Which database?", topic: "Storage", options: ["Postgres", "SQLite"], multi: false },
    { question: "Which tests?", topic: "Tests", options: ["Unit", "E2E"], multi: true },
    { question: "Anything else?", topic: "Notes", options: [], multi: false },
  ],
};

const mount = (onAnswer = vi.fn(), onSkip = vi.fn()) => {
  const out = render(<QuestionCard question={question} onAnswer={onAnswer} onSkip={onSkip} />);
  return { ...out, onAnswer, onSkip };
};

describe("QuestionCard", () => {
  it("says which question of how many", () => {
    mount();
    expect(screen.getByText("1 of 3")).toBeTruthy();
    expect(screen.getAllByText("Which database?").length).toBeGreaterThan(0);
  });

  it("keeps picks and typed text across a re-render", () => {
    const { rerender, onAnswer, onSkip } = mount();
    fireEvent.click(screen.getByText("SQLite"));
    fireEvent.click(screen.getByText("Next question"));
    fireEvent.change(screen.getByPlaceholderText("Or answer in your own words"), { target: { value: "and lint" } });
    // A state message with the same question comes in.
    rerender(<QuestionCard question={{ ...question }} onAnswer={onAnswer} onSkip={onSkip} />);
    expect((screen.getByPlaceholderText("Or answer in your own words") as HTMLInputElement).value).toBe("and lint");
    fireEvent.click(screen.getByText("Back"));
    expect(screen.getByText("SQLite").closest("button")!.className).toContain("on");
  });

  it("skips the questionnaire on a second tap", () => {
    const { onSkip } = mount();
    fireEvent.click(screen.getByText("Skip"));
    expect(onSkip).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("Skip all?"));
    expect(onSkip).toHaveBeenCalledTimes(1);
  });

  it("answers every question from the last one", () => {
    const { onAnswer } = mount();
    fireEvent.click(screen.getByText("Postgres"));
    fireEvent.click(screen.getByText("Next question"));
    expect(screen.getByText("2 of 3")).toBeTruthy();
    fireEvent.click(screen.getByText("Unit"));
    fireEvent.click(screen.getByText("E2E"));
    fireEvent.click(screen.getByText("Next question"));
    const send = screen.getByText("Answer").closest("button")!;
    expect(send.disabled).toBe(true);
    fireEvent.change(screen.getByPlaceholderText("Or answer in your own words"), { target: { value: "no" } });
    expect(send.disabled).toBe(false);
    fireEvent.click(send);
    expect(onAnswer).toHaveBeenCalledWith(["Postgres", "Unit, E2E", "no"]);
  });
});
