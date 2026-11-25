import { useContext } from "react";
import { LinkContext, useLink } from "../../lib/link";
import { useWidth } from "../../lib/useWidth";
import { PlusButton } from "../PlusMenu";
import { ComputerPicker } from "./ComputerPicker";
import "./Shell.css";

function Bar() {
  const canStart = useLink((s) => s.connected && s.places.length > 0);
  return (
    <header className="topbar">
      <ComputerPicker look="chip" />
      <PlusButton disabled={!canStart} />
    </header>
  );
}

/**
 * Over a phone's tab screens: the computer, and the + for a new session. A
 * tablet has both in the rail and a desktop in the sidebar, so there is none,
 * and neither before a computer is picked.
 */
export function TopBar() {
  const width = useWidth();
  const link = useContext(LinkContext);
  return width === "phone" && link ? <Bar /> : null;
}
