import { useId, useRef, type ReactNode } from "react";

interface Props {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  /// The label of the button that asks.
  children: ReactNode;
  /// For a change that removes access: the confirm button is red.
  danger?: boolean;
  disabled?: boolean;
}

/// A button that asks before it acts. The question is a modal <dialog>, and
/// the focus starts on Cancel, so Enter pressed out of habit changes nothing.
export function Confirm({ title, body, confirmLabel, onConfirm, children, danger, disabled }: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const id = useId();
  return (
    <>
      <button
        type="button"
        className="button"
        disabled={disabled}
        onClick={() => {
          dialog.current?.showModal();
          cancel.current?.focus();
        }}
      >
        {children}
      </button>
      <dialog ref={dialog} className="confirm" aria-labelledby={`${id}-title`} aria-describedby={`${id}-body`}>
        <h2 id={`${id}-title`}>{title}</h2>
        <div id={`${id}-body`} className="confirm-body">
          {body}
        </div>
        <div className="confirm-actions">
          <button ref={cancel} type="button" className="button" onClick={() => dialog.current?.close()}>
            Cancel
          </button>
          <button
            type="button"
            className={danger ? "button danger" : "button primary"}
            onClick={() => {
              dialog.current?.close();
              onConfirm();
            }}
          >
            {confirmLabel}
          </button>
        </div>
      </dialog>
    </>
  );
}
