import { Fragment, type ReactNode } from "react";
import { Icon } from "./icons";

// A small markdown: paragraphs, headings, lists, fenced and inline code, bold,
// italics, links, quotes, tables and pictures. Enough for what agents write.
// It follows md() and inline() of the old app.js, but builds React elements,
// so text from an agent is never parsed as HTML.

export interface MarkdownProps {
  src: string;
  /** A picture was tapped. */
  onPic?: (url: string, alt: string) => void;
  /** Open was pressed on a table. */
  onTable?: (table: HTMLTableElement) => void;
  /** Draws a closed fenced block as a chart or diagram; null leaves it as code. */
  onDiagram?: (lang: string, code: string) => ReactNode | null;
}

type Handlers = Pick<MarkdownProps, "onPic" | "onTable" | "onDiagram">;

export function Markdown({ src, ...handlers }: MarkdownProps) {
  return <>{blocks(String(src ?? ""), handlers)}</>;
}

const BULLET = /^\s*[-*+]\s+/;
const NUMBER = /^\s*\d+[.)]\s+/;
const MARK = /^\s*([-*+]|\d+[.)])\s+/;

function blocks(src: string, h: Handlers): ReactNode[] {
  const lines = src.split("\n");
  const out: ReactNode[] = [];
  let i = 0;
  const key = () => out.length;
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^\s*```(\w*)/);
    if (fence) {
      const code: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i++]);
      const closed = i < lines.length;
      i++;
      // Drawn only once the fence has closed: a block still streaming in
      // would redraw on every word.
      const text = code.join("\n");
      const drawn = closed && h.onDiagram ? h.onDiagram(fence[1], text) : null;
      out.push(
        drawn ? (
          <Fragment key={key()}>{drawn}</Fragment>
        ) : (
          <pre key={key()}>
            <code>{text}</code>
          </pre>
        ),
      );
      continue;
    }
    const head = line.match(/^(#{1,6})\s+(.*)/);
    if (head) {
      const Tag = `h${Math.min(3, head[1].length)}` as "h1" | "h2" | "h3";
      out.push(<Tag key={key()}>{inline(head[2], h)}</Tag>);
      i++;
      continue;
    }
    if (BULLET.test(line) || NUMBER.test(line)) {
      const ordered = NUMBER.test(line);
      const items: ReactNode[] = [];
      while (i < lines.length && (BULLET.test(lines[i]) || NUMBER.test(lines[i]))) {
        let item = lines[i].replace(MARK, "");
        i++;
        while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !MARK.test(lines[i])) {
          item += " " + lines[i].trim();
          i++;
        }
        items.push(<li key={items.length}>{inline(item, h)}</li>);
      }
      out.push(ordered ? <ol key={key()}>{items}</ol> : <ul key={key()}>{items}</ul>);
      continue;
    }
    if (/^\s*>/.test(line)) {
      const q: string[] = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) q.push(lines[i++].replace(/^\s*>\s?/, ""));
      out.push(<blockquote key={key()}>{blocks(q.join("\n"), h)}</blockquote>);
      continue;
    }
    if (/^\s*\|/.test(line) && i + 1 < lines.length && /^\s*\|?\s*:?-+/.test(lines[i + 1])) {
      const rows: string[] = [];
      while (i < lines.length && /^\s*\|/.test(lines[i])) rows.push(lines[i++]);
      out.push(<Table key={key()} rows={rows} h={h} />);
      continue;
    }
    if (line.trim() === "") {
      i++;
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i].trim() !== "" && !/^\s*(```|#{1,6}\s|[-*+]\s|\d+[.)]\s|>|\|)/.test(lines[i])) {
      para.push(lines[i++]);
    }
    if (para.length === 0) para.push(lines[i++]);
    out.push(<p key={key()}>{inline(para.join("\n"), h)}</p>);
  }
  return out;
}

const cells = (row: string) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());

// Figures line up on the right, as in a spreadsheet.
const isFigure = (c: string) => /^[-+−]?[$€£]?\s?[\d.,]+\s?(%|ms|s|x|k|M|GB|MB|KB)?$/.test(c);

// Short cells stay on one line; long ones get room to wrap into a few.
const cellClass = (c: string) => (isFigure(c) ? "num" : c.length > 28 ? "long" : undefined);

function Table({ rows, h }: { rows: string[]; h: Handlers }) {
  const head = cells(rows[0]);
  const n = rows.length - 2;
  return (
    <div className="tablewrap">
      <div className="tbar">
        <span>
          {n} row{n === 1 ? "" : "s"} · {head.length} columns
        </span>
        <button
          type="button"
          className="tablex"
          aria-label="Open the table"
          onClick={(ev) => {
            const table = ev.currentTarget.closest(".tablewrap")?.querySelector("table");
            if (table) h.onTable?.(table);
          }}
        >
          <Icon name="expand" />
          <span>Open</span>
        </button>
      </div>
      <div className="tscroll">
        <table>
          <thead>
            <tr>
              {head.map((c, j) => (
                <th key={j}>{inline(c, h)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.slice(2).map((r, j) => (
              <tr key={j}>
                {cells(r).map((c, k) => (
                  <td key={k} className={cellClass(c)}>
                    {inline(c, h)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// Inline spans. Like the old inline(), this runs the same passes in the same
// order, but a match becomes a marker pair (\u0002 n ... \u0003 n) instead of
// an HTML tag, and the markers turn into elements at the end. Codes and
// pictures leave first (\u0000 n, \u0001 n) so nothing inside them is touched.
interface Open {
  tag: "b" | "i" | "a";
  href?: string;
}

function inline(text: string, h: Handlers): ReactNode[] {
  const codes: string[] = [];
  const shots: { alt: string; url: string }[] = [];
  const opens: Open[] = [];
  const wrap = (tag: Open["tag"], inner: string, href?: string) => `\u0002${opens.push({ tag, href }) - 1}\u0002${inner}\u0003${opens.length - 1}\u0003`;

  let s = text.replace(/`([^`]+)`/g, (_, c: string) => `\u0000${codes.push(c) - 1}\u0000`);
  s = s.replace(/!\[([^\]]*)\]\(((?:\/|https?:\/\/|data:)[^\s)]+)\)/g, (_, alt: string, url: string) => `\u0001${shots.push({ alt, url }) - 1}\u0001`);
  s = s
    .replace(/\*\*(.+?)\*\*/g, (_, t: string) => wrap("b", t))
    .replace(/(^|\W)_(.+?)_(?=\W|$)/g, (_, pre: string, t: string) => pre + wrap("i", t))
    .replace(/(^|\W)\*(.+?)\*(?=\W|$)/g, (_, pre: string, t: string) => pre + wrap("i", t))
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, (_, t: string, url: string) => wrap("a", t, url));

  // Fold the markers into a tree. A close pops to its open, as a browser
  // would, so a crossing pair cannot leave an element open.
  type Frame = { open: number; kids: ReactNode[] };
  const stack: Frame[] = [{ open: -1, kids: [] }];
  const push = (n: ReactNode) => stack[stack.length - 1].kids.push(n);
  const close = () => {
    const f = stack.pop()!;
    const o = opens[f.open];
    const k = stack[stack.length - 1].kids;
    const kids = f.kids;
    k.push(
      o.tag === "b" ? (
        <b key={k.length}>{kids}</b>
      ) : o.tag === "i" ? (
        <i key={k.length}>{kids}</i>
      ) : (
        <a key={k.length} href={o.href} target="_blank" rel="noopener">
          {kids}
        </a>
      ),
    );
  };
  const text1 = (t: string) => {
    t.split("\n").forEach((part, j) => {
      if (j) push(<br key={`br${stack[stack.length - 1].kids.length}`} />);
      if (part) push(part);
    });
  };
  let last = 0;
  for (const m of s.matchAll(/\u0000(\d+)\u0000|\u0001(\d+)\u0001|\u0002(\d+)\u0002|\u0003(\d+)\u0003/g)) {
    text1(s.slice(last, m.index));
    last = m.index + m[0].length;
    const top = stack[stack.length - 1].kids;
    if (m[1] !== undefined) top.push(<code key={top.length}>{codes[+m[1]]}</code>);
    else if (m[2] !== undefined) {
      const { alt, url } = shots[+m[2]];
      top.push(<img key={top.length} src={url} alt={alt} className="pic" loading="lazy" onClick={h.onPic ? () => h.onPic!(url, alt) : undefined} />);
    } else if (m[3] !== undefined) stack.push({ open: +m[3], kids: [] });
    else {
      let at = stack.length - 1;
      while (at > 0 && stack[at].open !== +m[4]) at--;
      while (at > 0 && stack.length > at) close();
    }
  }
  text1(s.slice(last));
  while (stack.length > 1) close();
  return stack[0].kids;
}
