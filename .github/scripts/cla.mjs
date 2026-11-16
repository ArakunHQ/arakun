// CLA check (.github/workflows/cla.yml).
//
// Everyone who wrote a commit in a pull request, and the person who opened it,
// must have signed CLA.md. The signatures are a JSON file on the
// cla-signatures branch, in the format the contributor-assistant action used
// before, so the old signatures still count. People are matched by their
// GitHub user id, which stays the same when they rename their account.
//
// The result is a commit status on the head commit, not the job's own check,
// because a run from a comment can't put a check on the pull request. A status
// belongs to the commit, so it covers every open pull request with that head.
// A comment on each pull request says who still has to sign and how.
//
// This runs with a write token on pull requests from forks. It reads the pull
// request only through the API and never runs its code. Keep it that way.
//
// Env: GITHUB_TOKEN, GITHUB_REPOSITORY, GITHUB_EVENT_NAME, GITHUB_EVENT_PATH,
// optional GITHUB_API_URL, GITHUB_SERVER_URL, RUN_URL and PR (a pull request
// number for workflow_dispatch; empty checks every open pull request).

import { readFileSync } from "node:fs";

export const SENTENCE = "I have read the CLA Document and I hereby sign the CLA";
// The main ruleset requires a status with this name. It is the name the old
// action's job had, so the ruleset did not have to change.
const CONTEXT = "cla-assistant";
const BRANCH = "cla-signatures";
const FILE = "signatures/cla.json";
// Finds our own comment again, so a pull request gets one, kept up to date.
const MARKER = "<!-- arakun-cla -->";
// The copyright holder doesn't need a license from themselves.
const ALLOWED = new Set(["karloaldrete"]);

const normalize = (text) => text.toLowerCase().replace(/\s+/g, " ").trim();

/** A comment signs if it has the sentence anywhere, in any case and spacing. */
export function signs(body) {
  return normalize(body ?? "").includes(normalize(SENTENCE));
}

/** A comment asks for a new check if it has the word "recheck". */
export function rechecks(body) {
  return /\brecheck\b/i.test(body ?? "");
}

export function exempt(user) {
  return user.type === "Bot" || user.login.endsWith("[bot]") || ALLOWED.has(user.login.toLowerCase());
}

/**
 * Who has to sign for a pull request: its author and every commit author,
 * leaving out bots and the allow-list. `unknown` holds the commits whose
 * author email is not linked to a GitHub account, so no one can sign for them.
 */
export function signers(pr, commits) {
  const users = new Map();
  const unknown = [];
  const add = (user) => {
    if (user && !exempt(user)) users.set(user.id, user.login);
  };
  add(pr.user);
  for (const c of commits) {
    if (c.author?.id) add(c.author);
    else unknown.push({ sha: c.sha, name: c.commit.author?.name ?? "unknown" });
  }
  return { users, unknown };
}

/** The users in `users` (id -> login) who have no signature yet. */
export function unsigned(users, signatures) {
  const signed = new Set(signatures.signedContributors.map((s) => s.id));
  return [...users].filter(([id]) => !signed.has(id)).map(([id, login]) => ({ id, login }));
}

export function commentBody({ missing, unknown, truncated = false, claUrl }) {
  if (missing.length === 0 && unknown.length === 0 && !truncated) {
    return `${MARKER}\nEveryone in this pull request has signed the [CLA](${claUrl}). Thank you!\n`;
  }
  const lines = [
    MARKER,
    `Thank you for your pull request. Before it can be merged, everyone who wrote a commit in it must sign our [Contributor License Agreement](${claUrl}). You keep the copyright to your work.`,
    "",
  ];
  if (missing.length > 0) {
    lines.push(
      "To sign, read the CLA, then post this comment on this pull request:",
      "",
      "```",
      SENTENCE,
      "```",
      "",
      `Still to sign: ${missing.map((m) => `@${m.login}`).join(", ")}`,
      "",
    );
  }
  if (unknown.length > 0) {
    lines.push(
      "These commits have an author email that is not linked to a GitHub account, so no one can sign for them:",
      "",
      ...unknown.map((u) => `- ${u.sha.slice(0, 7)} (${u.name.replace(/[`@<>[\]]/g, "")})`),
      "",
      "Add the email to your GitHub account, or rewrite the commits with an email that is on it.",
      "",
    );
  }
  if (truncated) {
    lines.push(
      "This pull request has more than 250 commits, and GitHub lists only the first 250, so the authors of the rest can't be checked. Squash it into fewer commits, or ask a maintainer to check them by hand.",
      "",
    );
  }
  lines.push("Post `recheck` to check again.");
  return `${lines.join("\n")}\n`;
}

// --- GitHub API -------------------------------------------------------------

const API = process.env.GITHUB_API_URL || "https://api.github.com";
const REPO = process.env.GITHUB_REPOSITORY;
const TOKEN = process.env.GITHUB_TOKEN;

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

async function gh(method, path, body) {
  for (let attempt = 1; ; attempt++) {
    const res = await fetch(`${API}${path}`, {
      method,
      headers: {
        authorization: `Bearer ${TOKEN}`,
        accept: "application/vnd.github+json",
        "x-github-api-version": "2022-11-28",
        ...(body ? { "content-type": "application/json" } : {}),
      },
      body: body ? JSON.stringify(body) : undefined,
      // The signature file is read again to spot changes; never from a cache.
      cache: "no-store",
    });
    if (res.ok) return res.status === 204 ? null : res.json();
    if (attempt < 3 && (res.status >= 500 || res.status === 429)) {
      await new Promise((r) => setTimeout(r, 2000 * attempt));
      continue;
    }
    throw new HttpError(res.status, `${method} ${path}: ${res.status} ${await res.text()}`);
  }
}

async function paged(path) {
  const out = [];
  for (let page = 1; ; page++) {
    const items = await gh("GET", `${path}${path.includes("?") ? "&" : "?"}per_page=100&page=${page}`);
    out.push(...items);
    if (items.length < 100) return out;
  }
}

async function readSignatures() {
  try {
    const file = await gh("GET", `/repos/${REPO}/contents/${FILE}?ref=${BRANCH}`);
    return { data: JSON.parse(Buffer.from(file.content, "base64").toString("utf8")), sha: file.sha };
  } catch (e) {
    if (e.status === 404) return { data: { signedContributors: [] }, sha: undefined };
    throw e;
  }
}

/** Adds a signature. Returns false if the user had signed already. */
async function addSignature(user, comment, prNumber, repoId) {
  // The update names the file version it read, so two runs signing at once
  // can't overwrite each other: the second fails and reads again.
  for (let attempt = 1; ; attempt++) {
    const { data, sha } = await readSignatures();
    if (data.signedContributors.some((s) => s.id === user.id)) return false;
    data.signedContributors.push({
      name: user.login,
      id: user.id,
      comment_id: comment.id,
      created_at: comment.created_at,
      repoId,
      pullRequestNo: prNumber,
    });
    const content = Buffer.from(`${JSON.stringify(data, null, 2)}\n`).toString("base64");
    try {
      await gh("PUT", `/repos/${REPO}/contents/${FILE}`, {
        message: `@${user.login} has signed the CLA in ${REPO}#${prNumber}`,
        content,
        sha,
        branch: BRANCH,
      });
      return true;
    } catch (e) {
      if (attempt < 5 && (e.status === 409 || e.status === 422)) continue;
      throw e;
    }
  }
}

/** The commits GitHub lists for a pull request, and whether that is all of them. */
async function review(pr) {
  // GitHub lists at most 250 commits here. The rest can't be checked, so the
  // check fails rather than pass on the commits it saw.
  const commits = await paged(`/repos/${REPO}/pulls/${pr.number}/commits`);
  return { pr, ...signers(pr, commits), truncated: commits.length < pr.commits };
}

/**
 * The status for a commit that is the head of every pull request in `results`.
 * A status belongs to the commit, not the pull request, so it passes only if
 * it passes for each of them.
 */
export function verdict(results) {
  const missing = [...new Set(results.flatMap((r) => r.missing.map((m) => m.login)))];
  if (missing.length > 0) {
    const description = `Still to sign the CLA: ${missing.join(", ")}`;
    return { state: "failure", description: description.length > 140 ? `${description.slice(0, 137)}...` : description };
  }
  if (results.some((r) => r.unknown.length > 0)) {
    return { state: "failure", description: "Some commit authors have no GitHub account; see the comment." };
  }
  if (results.some((r) => r.truncated)) {
    return { state: "failure", description: "Over 250 commits: GitHub doesn't list them all, so check the authors by hand." };
  }
  return { state: "success", description: "Everyone has signed the CLA." };
}

async function updateComment(result, claUrl) {
  const { number } = result.pr;
  // Only comment on a pull request that needs it, and once there is a comment,
  // keep it current, so it says "signed" after the last signature.
  const comments = await paged(`/repos/${REPO}/issues/${number}/comments`);
  const ours = comments.find((c) => c.user.login === "github-actions[bot]" && c.body.startsWith(MARKER));
  const body = commentBody({ ...result, claUrl });
  if (ours) {
    if (ours.body !== body) await gh("PATCH", `/repos/${REPO}/issues/comments/${ours.id}`, { body });
  } else if (verdict([result]).state !== "success") {
    await gh("POST", `/repos/${REPO}/issues/${number}/comments`, { body });
  }
}

/** Sets the status of `sha` from the open pull requests that have it as their head. */
async function check(sha, open, claUrl) {
  const prs = open.filter((p) => p.head.sha === sha);
  if (prs.length === 0) {
    console.log(`${sha}: the head of no open pull request, skipped.`);
    return;
  }
  const reviews = await Promise.all(prs.map(review));
  // Runs don't wait for each other, so another run may record a signature
  // while this one checks. After posting, look at the file again: if it
  // changed, check again, so the last status posted saw the newest file.
  let before = await readSignatures();
  for (let attempt = 1; ; attempt++) {
    const results = reviews.map((r) => ({ ...r, missing: unsigned(r.users, before.data) }));
    const { state, description } = verdict(results);
    await gh("POST", `/repos/${REPO}/statuses/${sha}`, {
      state,
      context: CONTEXT,
      description,
      target_url: process.env.RUN_URL || undefined,
    });
    console.log(`${sha} (${prs.map((p) => `#${p.number}`).join(", ")}): ${state}, ${description}`);
    for (const result of results) await updateComment(result, claUrl);

    const after = await readSignatures();
    if (after.sha === before.sha) return;
    if (attempt === 5) throw new Error(`${sha}: the signatures kept changing; run the check again.`);
    before = after;
  }
}

async function main() {
  if (!REPO || !TOKEN) throw new Error("GITHUB_REPOSITORY and GITHUB_TOKEN are required");
  const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
  const name = process.env.GITHUB_EVENT_NAME;
  const server = process.env.GITHUB_SERVER_URL || "https://github.com";
  const repo = event.repository ?? (await gh("GET", `/repos/${REPO}`));
  const claUrl = `${server}/${REPO}/blob/${repo.default_branch}/CLA.md`;
  const open = await paged(`/repos/${REPO}/pulls?state=open`);
  const all = () => open.map((p) => p.head.sha);

  // The commits whose status may change.
  let shas;
  if (name === "pull_request_target") {
    // After a push or a close, the old head may still be the head of another
    // open pull request, whose status no longer has to include this one.
    shas = [event.pull_request.head.sha, event.before];
  } else if (name === "issue_comment") {
    if (!event.issue.pull_request) return;
    const { comment } = event;
    const pr = await gh("GET", `/repos/${REPO}/pulls/${event.issue.number}`);
    shas = [pr.head.sha];
    if (signs(comment.body)) {
      // Only someone this pull request needs can sign here, so a comment
      // can't sign for someone else.
      if ((await review(pr)).users.has(comment.user.id)) {
        if (await addSignature(comment.user, comment, pr.number, repo.id)) {
          console.log(`@${comment.user.login} signed the CLA.`);
          // Their other open pull requests were waiting for this too.
          shas = all();
        }
      } else {
        console.log(`@${comment.user.login} doesn't need to sign for #${pr.number}; ignored.`);
      }
    } else if (!rechecks(comment.body)) {
      return;
    }
  } else if (name === "workflow_dispatch") {
    const number = (process.env.PR ?? "").trim();
    if (number && !/^[0-9]+$/.test(number)) throw new Error(`Not a pull request number: ${number}`);
    shas = number ? [(await gh("GET", `/repos/${REPO}/pulls/${number}`)).head.sha] : all();
  } else {
    throw new Error(`Unexpected event: ${name}`);
  }

  let failed = false;
  for (const sha of new Set(shas.filter((s) => /^[0-9a-f]{40}$/.test(s ?? "") && !/^0+$/.test(s)))) {
    try {
      await check(sha, open, claUrl);
    } catch (e) {
      // One broken pull request must not leave the others unchecked.
      console.log(`::error::${sha}: ${e.message}`);
      failed = true;
    }
  }
  if (failed) process.exitCode = 1;
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
