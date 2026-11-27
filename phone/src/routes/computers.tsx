import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { CloudGroup } from "../components/CloudGroup";
import { NoticeBanners } from "../components/NoticeBanners";
import { TopBar } from "../components/shell/TopBar";
import { meQuery, pick, pickedMachine, useMachines, useRemoveMachine, useSaid, useSignOut } from "../lib/account";
import { CLOUD_FLAG, cloudRemoveQuestion, useCloudMachines, useMachineAction } from "../lib/cloud";
import { ago } from "../lib/format";
import { type Machine, RELAY } from "../lib/globals";
import { Icon } from "../lib/icons";
import { noteNoticeOpen, takePendingOpen } from "../lib/start";

// What a row says about when the computer was there.
function seen(m: Machine): string {
  if (m.online) return "online";
  const at = Number(m.last_seen_at) || 0;
  if (!at) return "offline";
  const when = ago(at);
  return `last seen ${when}${when === "now" ? "" : " ago"}`;
}

// The account host: the computers of the account, to pick one.
export function Computers() {
  const navigate = useNavigate();
  // A start that failed asks again by itself while the list is on screen.
  const me = useQuery({ ...meQuery, refetchInterval: (q) => (q.state.status === "error" ? 15_000 : false) });
  const boot = me.data && me.data.screen !== "signin" ? me.data : null;
  const machines = useMachines(!!boot);
  const remove = useRemoveMachine();
  const signOut = useSignOut();
  const said = useSaid();
  const flags = boot?.me?.flags;
  const flagged = Array.isArray(flags) && flags.includes(CLOUD_FLAG);
  // The cloud environment is in the list like a server; its row says what it is.
  const clouds = useCloudMachines(flagged).data ?? [];
  const cloud = new Set(clouds.map((m) => m.label));
  const cloudAct = useMachineAction();

  const got = machines.data;
  const blockedBy = got && "blocked" in got ? got : boot?.screen === "blocked" ? { reason: boot.reason ?? null } : null;
  const list: Machine[] = got && "list" in got ? got.list : boot?.machines || [];
  const login = boot?.me?.login;
  const sentence = blockedBy ? RELAY.sentence("blocked") + (blockedBy.reason ? ` ${blockedBy.reason}` : "") : said;

  const choose = (m: Machine) => {
    // The Computers tab lists the computer in use too: picking it again keeps its link.
    if (m.label !== pickedMachine()?.label) pick(m);
    const id = takePendingOpen();
    if (!id) return void navigate({ to: "/inbox" });
    noteNoticeOpen(id, list.length);
    void navigate({ to: "/s/$session", params: { session: id } });
  };
  const askRemove = (m: Machine) => {
    // Off the account alone, a cloud environment's container would stay and
    // come back to the list at its next start: it is deleted instead.
    const env = clouds.find((c) => c.label === m.label);
    if (env) {
      if (confirm(cloudRemoveQuestion(env.name))) cloudAct.mutate({ m: env, action: "remove" });
      return;
    }
    if (confirm(RELAY.removeQuestion(m.name))) remove.mutate(m);
  };
  const askSignOut = () => {
    if (signOut.isPending) return;
    if (confirm(`Sign out of ${login || "GitHub"} in this browser?`)) signOut.mutate();
  };

  let body = null;
  if (me.isError) {
    body = (
      <div className="empty">
        <img className="mascot" src="mascot.svg" alt="" />
        <div>{RELAY.unreachable()}</div>
        <button className="btn outline" onClick={() => location.reload()}>
          Try again
        </button>
      </div>
    );
  } else if (boot && !blockedBy) {
    const computers = list.length ? (
      <section className="group">
        <div className="glabel">Computers</div>
        <div className="gbox">
          {list.map((m) => (
            // The pick and Remove sit side by side in the row: a button cannot hold another.
            <div className="prow mrow" key={m.label}>
              <button className="mpick" onClick={() => choose(m)}>
                <span className="ricon">
                  <Icon name={cloud.has(m.label) ? "cloud" : "monitor"} size={20} />
                </span>
                <span className="rbody">
                  <span className="rt">{m.name}</span>
                  <span className="rs">{cloud.has(m.label) ? "Cloud environment" : m.kind === "server" ? "Server" : "Arakun app"}</span>
                </span>
                <span className={`badge${m.online ? " idle" : ""}`}>
                  <span className="dot" />
                  {seen(m)}
                </span>
              </button>
              <button className="btn ghost small" aria-label={`Remove ${m.name}`} onClick={() => askRemove(m)}>
                Remove
              </button>
            </div>
          ))}
        </div>
      </section>
    ) : (
      <div className="empty">
        <img className="mascot" src="mascot.svg" alt="" />
        <div>No computers yet. Sign in on a computer running Arakun.</div>
      </div>
    );
    body = (
      <>
        {computers}
        <CloudGroup flagged={flagged} />
      </>
    );
  }

  return (
    <div className="screen">
      {/* The bar is empty here: it only keeps the header clear of the notch. */}
      <div className="statusbar" />
      <TopBar />
      <div className="hdr">
        <div className="grow">
          <h1>Computers</h1>
          {login ? <div className="sub">Signed in as {login}</div> : null}
        </div>
        {login ? (
          <button className="btn ghost" onClick={askSignOut}>
            Sign out
          </button>
        ) : null}
      </div>
      <div className="settings">
        <NoticeBanners notices={boot?.me?.notices || []} />
        {!me.isError && sentence ? <div className="err">{sentence}</div> : null}
        {body}
      </div>
    </div>
  );
}
