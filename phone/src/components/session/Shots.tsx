// Photos waiting in the composer: a thumbnail each, a tap takes it out.

import type { Shot } from "../../lib/shots";
import "./Shots.css";

export function Shots({ shots, onRemove }: { shots: Shot[]; onRemove: (index: number) => void }) {
  if (!shots.length) return null;
  return (
    <div id="shots">
      {shots.map((sh, i) => (
        <button key={i} type="button" className="shotchip" aria-label="Remove photo" onClick={() => onRemove(i)}>
          <img src={sh.thumb} alt="" />
          <b>×</b>
        </button>
      ))}
    </div>
  );
}
