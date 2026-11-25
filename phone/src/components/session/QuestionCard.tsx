// The agent's questionnaire, one question at a time ("1 of 3"), with Skip and
// Next or Answer always in reach under it. Each question has its options (one
// or several, as it allows) and a line for an answer in your own words. Picks
// and typed words live in this card's state until it is sent; the parent keys
// it by the question's id, so a state message does not reset them. The card
// folds to its header so the log underneath can be read first.

import { useState } from "react";
import { Icon } from "../../lib/icons";
import { useArmed } from "./PermissionCard";

export interface Question {
  tool_call_id: string;
  questions: { question: string; topic?: string; options: string[]; multi?: boolean }[];
}

export interface QuestionCardProps {
  question: Question;
  onAnswer: (answers: string[]) => void;
  onSkip: () => void;
}

export function QuestionCard({ question: q, onAnswer, onSkip }: QuestionCardProps) {
  const total = q.questions.length;
  const [picked, setPicked] = useState<number[][]>(() => q.questions.map(() => []));
  const [typed, setTyped] = useState<string[]>(() => q.questions.map(() => ""));
  const [folded, setFolded] = useState(false);
  const [page, setPage] = useState(0);
  const [skipArmed, tapSkip] = useArmed(3000);

  const i = Math.min(page, total - 1);
  const qq = q.questions[i];
  const last = i === total - 1;
  const answerOf = (k: number) => {
    const parts = (picked[k] ?? []).map((o) => q.questions[k].options[o]);
    const own = (typed[k] ?? "").trim();
    if (own) parts.push(own);
    return parts.join(", ");
  };
  const answers = () => q.questions.map((_, k) => answerOf(k));
  const ready = last ? answers().every((a) => a.length > 0) : answerOf(i).length > 0;

  const pick = (j: number) => {
    const mine = picked[i];
    const at = mine.indexOf(j);
    const next = qq.multi ? (at >= 0 ? mine.filter((x) => x !== j) : [...mine, j]) : at >= 0 ? [] : [j];
    setPicked(picked.map((p, k) => (k === i ? next : p)));
  };
  const go = () => {
    if (!ready) return;
    if (!last) setPage(i + 1);
    else onAnswer(answers());
  };

  return (
    <div className={`ask${folded ? " folded" : ""}`}>
      <button type="button" className="ahead" aria-expanded={!folded} onClick={() => setFolded(!folded)}>
        <span className="label">Question</span>
        <span className="peek">{qq.question}</span>
        {total > 1 ? (
          <span className="n">
            <span className="segs">
              {q.questions.map((_, k) => (
                <i key={k} className={k <= i ? "on" : ""} />
              ))}
            </span>
            {i + 1} of {total}
          </span>
        ) : null}
        <Icon name="chevron" />
      </button>
      <div className="body">
        <div className="q">
          {qq.topic && total > 1 ? <div className="topic">{qq.topic}</div> : null}
          <div className="text">{qq.question}</div>
          {qq.multi ? <div className="multi">Pick any that apply</div> : null}
          <div className="opts">
            {qq.options.map((o, j) => (
              <button key={j} type="button" className={`opt${picked[i].includes(j) ? " on" : ""}${qq.multi ? " multi" : ""}`} onClick={() => pick(j)}>
                <span className="mark" />
                <span>{o}</span>
              </button>
            ))}
          </div>
          <input
            className="own"
            placeholder="Or answer in your own words"
            value={typed[i]}
            autoComplete="off"
            enterKeyHint={last ? "send" : "next"}
            onChange={(ev) => setTyped(typed.map((t, k) => (k === i ? ev.target.value : t)))}
            onKeyDown={(ev) => {
              if (ev.key !== "Enter" || ev.nativeEvent.isComposing) return;
              ev.preventDefault();
              go();
            }}
          />
        </div>
      </div>
      <div className="actions">
        {/* Skipping drops the whole questionnaire, so it asks to be sure. */}
        <button type="button" className={`btn outline${skipArmed ? " armed" : ""}`} onClick={() => tapSkip() && onSkip()}>
          {skipArmed ? "Skip all?" : "Skip"}
        </button>
        {i > 0 ? (
          <button type="button" className="btn outline" onClick={() => setPage(i - 1)}>
            Back
          </button>
        ) : null}
        <button type="button" className="btn grow" disabled={!ready} onClick={go}>
          {last ? "Answer" : "Next question"}
        </button>
      </div>
    </div>
  );
}
