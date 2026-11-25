import { type Notice, useMarkNoticeRead } from "../lib/account";
import { RELAY } from "../lib/globals";
import { Icon } from "../lib/icons";

/** The operator's notices for this account, until dismissed. */
export function NoticeBanners({ notices }: { notices: Notice[] }) {
  const read = useMarkNoticeRead();
  return (
    <>
      {notices.map((n) => (
        <div className="setup" key={String(n.id)}>
          <span className="sicon">
            <Icon name="bell" />
          </span>
          <span className="body">
            <div className="t">{n.text}</div>
            {n.url && RELAY.noticeLink(n.url) ? (
              <div className="s">
                <a href={n.url} target="_blank" rel="noopener">
                  {n.url}
                </a>
              </div>
            ) : null}
          </span>
          <button className="x" aria-label="Dismiss" onClick={() => read.mutate(n.id)}>
            <Icon name="close" />
          </button>
        </div>
      ))}
    </>
  );
}
