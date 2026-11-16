// CLA check (.github/workflows/cla.yml).
//
// Everyone who wrote a commit in a pull request, and the person who opened it,
// must have signed CLA.md. The signatures are a JSON file on the
// cla-signatures branch, in the format the contributor-assistant action used
// before, so the old signatures still count. People are matched by their
// GitHub user id, which stays the same when they rename their account.
//
// The result is a commit status on the head commit, not the job's own check,
// because a run from a comment can't put a check on the pull request. A
// comment on the pull request says who still has to sign and how.
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

export function commentBody({ missing, unknown, claUrl }) {
  if (missing.length === 0 && unknown.length === 0) {
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

async function check(number, signatures, claUrl) {
  const pr = await gh("GET", `/repos/${REPO}/pulls/${number}`);
  if (pr.state !== "open") {
    console.log(`#${number}: closed, skipped.`);
    return;
  }
  // This endpoint lists at most 250 commits. A pull request that big is
  // reviewed by hand anyway.
  const commits = await paged(`/repos/${REPO}/pulls/${number}/commits`);
  const { users, unknown } = signers(pr, commits);
  const missing = unsigned(users, signatures);

  let state = "success";
  let description = "Everyone has signed the CLA.";
  if (missing.length > 0) {
    state = "failure";
    description = `Still to sign the CLA: ${missing.map((m) => m.login).join(", ")}`;
  } else if (unknown.length > 0) {
    state = "failure";
    description = "Some commit authors have no GitHub account; see the comment.";
  }
  if (description.length > 140) description = `${description.slice(0, 137)}...`;
  await gh("POST", `/repos/${REPO}/statuses/${pr.head.sha}`, {
    state,
    context: CONTEXT,
    description,
    target_url: process.env.RUN_URL || undefined,
  });
  console.log(`#${number} ${pr.head.sha}: ${state}, ${description}`);

  // Only comment on a pull request that needs it, and once there is a comment,
  // keep it current, so it says "signed" after the last signature.
  const comments = await paged(`/repos/${REPO}/issues/${number}/comments`);
  const ours = comments.find((c) => c.user.login === "github-actions[bot]" && c.body.startsWith(MARKER));
  const body = commentBody({ missing, unknown, claUrl });
  if (ours) {
    if (ours.body !== body) await gh("PATCH", `/repos/${REPO}/issues/comments/${ours.id}`, { body });
  } else if (state !== "success") {
    await gh("POST", `/repos/${REPO}/issues/${number}/comments`, { body });
  }
}

async function main() {
  if (!REPO || !TOKEN) throw new Error("GITHUB_REPOSITORY and GITHUB_TOKEN are required");
  const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
  const name = process.env.GITHUB_EVENT_NAME;
  const server = process.env.GITHUB_SERVER_URL || "https://github.com";
  const repo = event.repository ?? (await gh("GET", `/repos/${REPO}`));
  const claUrl = `${server}/${REPO}/blob/${repo.default_branch}/CLA.md`;

  let numbers;
  if (name === "pull_request_target") {
    numbers = [event.pull_request.number];
  } else if (name === "issue_comment") {
    if (!event.issue.pull_request) return;
    const { comment } = event;
    numbers = [event.issue.number];
    if (signs(comment.body)) {
      // Only someone this pull request needs can sign here, so a comment
      // can't sign for someone else.
      const pr = await gh("GET", `/repos/${REPO}/pulls/${event.issue.number}`);
      const commits = await paged(`/repos/${REPO}/pulls/${event.issue.number}/commits`);
      if (signers(pr, commits).users.has(comment.user.id)) {
        if (await addSignature(comment.user, comment, event.issue.number, repo.id)) {
          console.log(`@${comment.user.login} signed the CLA.`);
          // Their other open pull requests were waiting for this too.
          numbers = (await paged(`/repos/${REPO}/pulls?state=open`)).map((p) => p.number);
        }
      } else {
        console.log(`@${comment.user.login} doesn't need to sign for #${event.issue.number}; ignored.`);
      }
    } else if (!rechecks(comment.body)) {
      return;
    }
  } else if (name === "workflow_dispatch") {
    const pr = (process.env.PR ?? "").trim();
    if (pr && !/^[0-9]+$/.test(pr)) throw new Error(`Not a pull request number: ${pr}`);
    numbers = pr ? [Number(pr)] : (await paged(`/repos/${REPO}/pulls?state=open`)).map((p) => p.number);
  } else {
    throw new Error(`Unexpected event: ${name}`);
  }

  const { data: signatures } = await readSignatures();
  let failed = false;
  for (const number of numbers) {
    try {
      await check(number, signatures, claUrl);
    } catch (e) {
      // One broken pull request must not leave the others unchecked.
      console.log(`::error::#${number}: ${e.message}`);
      failed = true;
    }
  }
  if (failed) process.exitCode = 1;
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
