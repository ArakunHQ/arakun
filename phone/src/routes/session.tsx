// One session: what it said, what it asks, and the box to answer in. The
// parts live under components/session; this screen holds what they share
// (the draft, the photos, what is on its way, the sheet on top) and talks to
// the link.

import { useNavigate, useParams, useRouter, useSearch } from "@tanstack/react-router";
import { type ReactNode, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { AgentAvatar } from "../components/SessionRow";
import { Composer } from "../components/session/Composer";
import { Counts, EntryContext, type EntryHandlers } from "../components/session/Entry";
import { Pending, type PendingMessage, sentId, usePending } from "../components/session/Pending";
import { type Permission, PermissionCard } from "../components/session/PermissionCard";
import { type Question, QuestionCard } from "../components/session/QuestionCard";
import { type SecretRequest, SecretCard } from "../components/session/SecretCard";
import { Sheet } from "../components/session/Sheet";
import { useSheetNav, validateSessionSearch } from "../components/session/sheetNav";
import { BackgroundLine, SubagentContext, type SubagentHandlers } from "../components/session/Subagents";
import { SubagentView } from "../components/session/SubagentView";
import { usageSummary } from "../components/session/StopRow";
import { Transcript, type TranscriptHandle } from "../components/session/Transcript";
import { drawDiagram, NodeView, Pic, PicView, usePics } from "../components/session/Views";
import { StatusBar } from "../components/StatusBar";
import { keepDraft, loadDraft } from "../lib/drafts";
import { statusLabel } from "../lib/format";
import { Icon } from "../lib/icons";
import { type Link, LinkContext, useLink } from "../lib/link";
import type { ClientMessage, Session } from "../lib/model";
import { setMuted, useCanMute, useMuted } from "../lib/mute";
import { sendShot, type Shot } from "../lib/shots";
import { mode, openSession } from "../lib/start";
import { toast } from "../lib/toast";
import { useWidth } from "../lib/useWidth";
import { refit } from "../lib/viewport";
import { OfflineCard } from "./offline";
import "./session.css";

// The session this screen shows, while it is mounted. Leaving closes the
// session on the computer, but only once it is sure the screen is not back
// for the same one: StrictMode unmounts and mounts again at once, and a
// notification can move the screen straight to another session.
let shown: string | null = null;

function useOpen(link: Link, id: string) {
  useEffect(() => {
    shown = id;
    // A row tap has opened it already; a deep link or a notification has not.
    if (link.getState().session?.id !== id) openSession(link, id);
    return () => {
      shown = null;
      setTimeout(() => {
        if (shown !== id && link.getState().session?.id === id) link.send({ type: "close" });
      }, 0);
    };
  }, [link, id]);
}

export function SessionScreen() {
  const { session: id } = useParams({ from: "/s/$session" });
  const link = useContext(LinkContext);
  if (!link) return null;
  return <Opened key={id} link={link} id={id} />;
}

type View = { kind: "pic"; src: string; alt: string } | { kind: "node"; title: string; node: Node } | null;

function Opened({ link, id }: { link: Link; id: string }) {
  useOpen(link, id);
  const s = useLink((st) => (st.session?.id === id ? st.session : null));
  if (!s) {
    return (
      <div className="screen">
        <StatusBar />
      </div>
    );
  }
  return <SessionView link={link} s={s} />;
}

function SessionView({ link, s }: { link: Link; s: Session }) {
  const router = useRouter();
  const navigate = useNavigate();
  // Next to the sidebar or the rail, they are the way out: no Back.
  const alone = useWidth() === "phone";
  const row = useLink((st) => st.inbox.find((r) => r.id === s.id));
  const connected = useLink((st) => st.connected);
  const muted = useMuted().includes(s.id);
  const canMute = useCanMute();
  const [draft, setDraftState] = useState(() => loadDraft(s.id));
  const [shots, setShots] = useState<Shot[]>([]);
  const [view, setView] = useState<View>(null);
  // Answered here; the card goes before the computer says so.
  const [answered, setAnswered] = useState<string[]>([]);
  const [askedOlder, setAskedOlder] = useState<number | null>(null);
  const { pending, add, drop, take } = usePending(link, s);
  const transcript = useRef<TranscriptHandle>(null);
  const pics = usePics(mode() === "account");
  const send = useCallback((m: ClientMessage) => link.send(m), [link]);
  const { sheet, go: openSheet } = useSheetNav(s.id, send);
  const agents = useSubagents(link, s.id);

  const setDraft = (text: string) => {
    setDraftState(text);
    keepDraft(s.id, text);
  };

  // The inbox row knows the title and the status first.
  const title = row?.title || s.title;
  const status = row?.status ?? s.status;
  const place = row ? (row.project ?? "") + (row.env && row.env !== row.project ? ` · ${row.env}` : "") : s.place;
  const live = useMemo(() => ({ ...s, title, status, place }), [s, title, status, place]);
  const summary = usageSummary(s);
  // While the agent works, the stop row carries the cost.
  const sub = [place, s.agent, status !== "working" ? summary : ""].filter(Boolean).join(" · ");
  const changes = s.changes as { files?: number; added?: number; removed?: number } | undefined;

  // An answer only counts once the computer has it: offline the card stays, and says why.
  const stillLinked = () => {
    if (connected) return true;
    toast("Not connected. Try again once it reconnects.");
    return false;
  };

  // Out to the computer, or held here until it is back: `typed` with the
  // photos `pics`, under a new id the computer names if it does not get there.
  const deliver = (typed: string, pics: Shot[], now?: boolean) => {
    // Without the computer the message waits on the phone and goes out on
    // reconnect; photos cannot, they need the socket now.
    const queued = !connected;
    if (queued && pics.length) {
      toast("Photos wait for a connection");
      return false;
    }
    const sent = sentId();
    const msg: ClientMessage = { type: "send", session: s.id, text: typed, sent };
    if (now) msg.now = true;
    if (pics.length) {
      msg.uploads = pics.map((sh, i) => {
        const up = `${Date.now()}-${i}-${Math.random().toString(36).slice(2, 7)}`;
        sendShot(sh, up, s.id, send);
        return up;
      });
      if (!typed) msg.text = pics.length === 1 ? "(photo)" : `(${pics.length} photos)`;
    }
    if (!queued) send(msg);
    add({ text: String(msg.text), after: s.from + s.entries.length, queued, sent, typed, shots: pics.length ? pics : undefined });
    transcript.current?.scrollToEnd();
    return true;
  };

  const submit = ({ now, text }: { now?: boolean; text?: string }) => {
    const body = (text ?? draft).trim();
    if (!body && !shots.length) return;
    if (now && !stillLinked()) return;
    if (!deliver(body, shots, now)) return;
    setShots([]);
    setDraft("");
    // The keyboard goes down: the page gets its height back, with no gap under.
    refit(0, 120, 300, 600);
  };

  // A message that did not reach a server's session goes again, as new.
  const retry = (p: PendingMessage) => {
    if (!p.sent || (!connected && p.shots?.length)) {
      if (p.shots?.length) toast("Photos wait for a connection");
      return;
    }
    if (take(p.sent)) deliver(p.typed ?? p.text, p.shots ?? []);
  };

  // Or back into the box, after anything written there, with its photos.
  const takeBackUnsent = (p: PendingMessage) => {
    if (!p.sent || !take(p.sent)) return;
    const typed = p.typed ?? p.text;
    if (typed) setDraft(draft.trim() ? `${draft.trimEnd()}\n\n${typed}` : typed);
    if (p.shots?.length) setShots((all) => [...all, ...(p.shots ?? [])]);
  };

  const cancel = () => {
    if (!stillLinked()) return;
    // The queue waits, paused, as Esc leaves it in the app.
    send({ type: "cancel", session: s.id, keep_queue: true });
    toast("Stopping…");
  };

  // Out of the queue and into the box, after anything written there.
  const takeBack = (index: number, text: string) => {
    if (!stillLinked()) return;
    send({ type: "unqueue", session: s.id, index, text });
    drop(text);
    setDraft(draft.trim() ? `${draft.trimEnd()}\n\n${text}` : text);
  };

  const resumeQueue = () => {
    if (!stillLinked()) return;
    send({ type: "resume_queue", session: s.id });
  };

  const back = () => {
    if (router.history.canGoBack()) router.history.back();
    else void navigate({ to: "/inbox" });
  };

  const loadOlder = useCallback(() => {
    if (s.from === 0) return;
    setAskedOlder(s.from);
    send({ type: "entries", session: s.id, from: Math.max(0, s.from - 100), to: s.from });
  }, [send, s.id, s.from]);
  // Asked for what comes before `from`; the answer moves `from`.
  const loadingOlder = askedOlder === s.from;

  // Tapping a picture opens it over everything, fetched first if need be.
  const showPic = useCallback(
    (url: string, alt: string) =>
      void pics.load(url).then((src) => {
        if (src) setView({ kind: "pic", src, alt });
      }),
    [pics],
  );
  const openNode = useCallback((title: string, node: Node) => setView({ kind: "node", title, node }), []);
  const handlers: EntryHandlers = useMemo(
    () => ({
      renderPic: (url, alt) => <Pic store={pics} url={url} alt={alt} onTap={showPic} />,
      onPic: showPic,
      onTable: (table) => openNode("Table", table.cloneNode(true)),
      onDiagram: (lang, code) => drawDiagram(lang, code, openNode),
      openChanges: () => openSheet({ kind: "changes" }),
    }),
    [pics, showPic, openNode, openSheet],
  );

  // Retry and Take back read the state of the moment they are tapped.
  const unsent = useRef({ retry, takeBack: takeBackUnsent });
  unsent.current = { retry, takeBack: takeBackUnsent };
  // Stable, so typing in the box does not draw the transcript again.
  const pendingNode = useMemo(
    () => <Pending pending={pending} queued={s.queue} onRetry={(p) => unsent.current.retry(p)} onTakeBack={(p) => unsent.current.takeBack(p)} />,
    [pending, s.queue],
  );
  const keepAsCut = useCallback((text: string) => openSheet({ kind: "cut", seed: { text }, fromList: false }), [openSheet]);

  const p = s.permission as Permission | null;
  const q = s.question as Question | null;
  const secret = s.secret as SecretRequest | null | undefined;
  const answer = (m: ClientMessage, toolCallId: string) => {
    if (!stillLinked()) return false;
    send(m);
    setAnswered((a) => [...a, toolCallId]);
    return true;
  };

  let viewNode: ReactNode = null;
  if (view?.kind === "pic") viewNode = <PicView src={view.src} alt={view.alt} onClose={() => setView(null)} />;
  else if (view?.kind === "node") viewNode = <NodeView title={view.title} node={view.node} onClose={() => setView(null)} />;

  return (
    <div className="screen" data-session={s.id}>
      <StatusBar />
      <div className="hdr">
        {alone ? (
          <button type="button" className="iconbtn" aria-label="Back" onClick={back}>
            <Icon name="back" />
          </button>
        ) : null}
        <AgentAvatar agent={s.agent} />
        <div className="grow">
          <div className="title">{title || "New session"}</div>
          <div className="sub">{sub}</div>
        </div>
        <span className={`badge ${status}`}>
          <span className="dot" />
          <span>{statusLabel(status)}</span>
        </span>
        {canMute ? (
          <button type="button" className="iconbtn" aria-label={muted ? "Unmute" : "Mute"} aria-pressed={muted} onClick={() => void setMuted(link, s.id, !muted)}>
            <Icon name={muted ? "bell-off" : "bell"} />
          </button>
        ) : null}
        <button type="button" className="iconbtn" aria-label="Model and reasoning" onClick={() => openSheet({ kind: "settings" })}>
          <Icon name="tune" />
        </button>
      </div>
      {/* What is read and written: a column in the middle of a desktop. */}
      <div className="scol">
        <OfflineCard link={link} />
        {changes?.files ? (
          <button type="button" className="chg" onClick={() => openSheet({ kind: "changes" })}>
            <Icon name="diff" />
            <span>
              {changes.files} file{changes.files === 1 ? "" : "s"} changed
            </span>
            <Counts added={changes.added} removed={changes.removed} />
          </button>
        ) : null}
        <div className="older" style={s.from > 0 ? undefined : { display: "none" }}>
          {s.from > 0 ? (loadingOlder ? "Loading…" : `${s.from} earlier`) : ""}
        </div>
        <EntryContext.Provider value={handlers}>
          <SubagentContext.Provider value={agents.inSession}>
            <Transcript
              ref={transcript}
              session={s}
              loadingOlder={loadingOlder}
              onLoadOlder={loadOlder}
              pending={pendingNode}
              onHoldBubble={keepAsCut}
            />
          </SubagentContext.Provider>
        </EntryContext.Provider>
        {p && !answered.includes(p.tool_call_id) ? (
          <PermissionCard
            key={p.tool_call_id}
            permission={p}
            place={place}
            tool={s.entries.find((e) => e.type === "tool" && e.id === p.tool_call_id)}
            onAnswer={(option) => {
              const o = p.options.find((x) => x.id === option);
              if (answer({ type: "permission", session: s.id, option }, p.tool_call_id) && o) toast(o.kind.startsWith("allow") ? `${o.name} ✓` : o.name);
            }}
          />
        ) : null}
        {q && !answered.includes(q.tool_call_id) ? (
          <QuestionCard
            key={q.tool_call_id}
            question={q}
            onAnswer={(answers) => answer({ type: "answer", session: s.id, answers }, q.tool_call_id)}
            onSkip={() => answer({ type: "skip_question", session: s.id }, q.tool_call_id)}
          />
        ) : null}
        {secret && !answered.includes(`secret:${secret.id}`) ? (
          <SecretCard
            key={secret.id}
            secret={secret}
            onSave={(values) => {
              // The computer refuses it too; this keeps it off the network.
              if (secret.private) answer({ type: "secret", session: s.id, id: secret.id, values }, `secret:${secret.id}`);
            }}
            onDecline={() => answer({ type: "secret", session: s.id, id: secret.id }, `secret:${secret.id}`)}
          />
        ) : null}
        <BackgroundLine subagents={agents.list} status={status} />
        <Composer
          session={live}
          draft={draft}
          setDraft={setDraft}
          shots={shots}
          setShots={setShots}
          onSubmit={submit}
          onCancel={cancel}
          onTakeBack={takeBack}
          onResumeQueue={resumeQueue}
          onOpenCuts={() => openSheet({ kind: "cuts" })}
        />
      </div>
      {sheet ? (
        <div id="sheet">
          <Sheet sheet={sheet} session={live} send={send} open={openSheet} draft={draft} />
        </div>
      ) : null}
      {agents.open ? (
        <EntryContext.Provider value={handlers}>
          <SubagentContext.Provider value={agents.inOpen}>
            <SubagentView
              agent={s.agent}
              subagent={agents.list.find((a) => a.id === agents.open?.id)}
              transcript={agents.open}
              loadingOlder={agents.loadingOlder}
              onLoadOlder={agents.loadOlder}
              onBack={agents.back}
            />
          </SubagentContext.Provider>
        </EntryContext.Provider>
      ) : null}
      {viewNode}
    </div>
  );
}

/**
 * The session's subagents, and the one whose transcript is open: it is
 * `?subagent=` on the session's address, so the phone's back gesture goes
 * back to the session. The computer keeps the open one live until it closes.
 */
function useSubagents(link: Link, id: string) {
  const router = useRouter();
  const navigate = useNavigate();
  const want = useSearch({ strict: false, select: (q: Record<string, unknown>) => validateSessionSearch(q).subagent ?? null });
  const list = useLink((st) => st.subagents);
  const entries = useLink((st) => st.session?.entries ?? NONE);
  const watched = useLink((st) => st.subagent);
  const open = want && watched?.id === want ? watched : null;
  // Asked for what comes before `from` of this one; the answer moves `from`.
  const [askedOlder, setAskedOlder] = useState("");

  useEffect(() => {
    if (!want) return;
    link.send({ type: "subagent", session: id, subagent: want });
    return () => link.send({ type: "subagent", session: id });
  }, [link, id, want]);

  const onOpen = useCallback(
    (sub: string) =>
      void navigate({ to: "/s/$session", params: { session: id }, search: { subagent: sub }, state: { subagentFrom: true } }),
    [navigate, id],
  );
  const back = useCallback(() => {
    if (router.state.location.state.subagentFrom) router.history.back();
    else void navigate({ to: "/s/$session", params: { session: id }, search: {}, replace: true });
  }, [router, navigate, id]);
  const from = open?.from ?? 0;
  const loadOlder = useCallback(() => {
    if (!open || from === 0) return;
    setAskedOlder(`${open.id}:${from}`);
    link.send({ type: "entries", session: id, subagent: open.id, from: Math.max(0, from - 50), to: from });
  }, [link, id, open, from]);

  const inSession: SubagentHandlers = useMemo(() => ({ subagents: list, entries, open: null, onOpen }), [list, entries, onOpen]);
  const inOpen: SubagentHandlers = useMemo(() => ({ subagents: list, entries: open?.entries ?? NONE, open, onOpen }), [list, open, onOpen]);
  return { list, open, inSession, inOpen, back, loadOlder, loadingOlder: !!open && open.from > 0 && askedOlder === `${open.id}:${open.from}` };
}

const NONE: never[] = [];
