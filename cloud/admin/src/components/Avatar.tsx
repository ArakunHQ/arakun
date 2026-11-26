import { avatarUrl } from "../format";

/// The avatar of the GitHub user linked to an account. An account with none
/// keeps the place empty, so rows still line up.
export function Avatar({ githubId, size }: { githubId: number | null; size: number }) {
  const src = avatarUrl(githubId, size);
  return src ? <img className="avatar" src={src} alt="" /> : <span className="avatar" aria-hidden="true" />;
}
