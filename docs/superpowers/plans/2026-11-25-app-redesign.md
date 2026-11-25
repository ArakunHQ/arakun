# app.arakun.dev Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the phone page (`phone/`) to React with TanStack Router and TanStack Query, apply the brand book, and lay it out for phone, tablet and desktop.

**Architecture:** Vite builds `phone/src` into `phone/dist`, which is committed and served by both the Rust app (`rust_embed`) and the Worker (`[assets]`). The crypto scripts (`e2e.js`, `relay.js`, `diagrams.js`) stay plain scripts in `phone/public/`, loaded before the bundle and reached through typed wrappers. Request and response goes through TanStack Query; the live link is a small external store read with `useSyncExternalStore`; routing is TanStack Router with hash history.

**Tech Stack:** React 19, TypeScript, Vite, TanStack Router, TanStack Query v5, Vitest with jsdom and Testing Library, plain CSS.

**Spec:** `docs/superpowers/specs/2026-11-25-app-redesign-design.md` (issue #808). Mockups: https://doop.arakun.dev/c/sEA9hKJMVj?page=KECtakcOwO

## Global Constraints

- Three stacked pull requests (use the `stack-pr` skill, branches `pr/phone-react`, `pr/phone-brand`, `pr/phone-layouts`), titles:
  1. `refactor(phone): port the phone page to React and TanStack`
  2. `feat(phone): apply the brand book to the phone page`
  3. `feat(phone): lay out the phone page for tablet and desktop`
- `phone/public/e2e.js`, `relay.js`, `diagrams.js` are moved with `git mv` and not edited (the vectors in `phone/test/vectors` are shared with Rust, iOS and Android).
- `phone/dist` is committed. Fixed file names, no content hashes, `base: "./"`. Build target `safari16.4`, `chrome111`. No inline `<script>` in the output (the Worker's CSP is `script-src 'self'`).
- Router: hash history. `#s=<id>` redirects to `#/s/<id>`.
- The permission card keeps "Hold to allow" (700 ms, `navigator.vibrate(15)` on success, "Hold to allow" notice on a plain tap) and "Always allow" needs a second tap within 3000 ms ("Tap again to always allow"), at every width.
- Breakpoints: phone `< 768px`, tablet `768px–1199px`, desktop `>= 1200px`. Desktop sidebar 288 px, session column 760 px, tablet rail 64 px, touch targets 44 px below 1200 px.
- Words: the "Phone" tab becomes "Settings" everywhere. Phone tabs: Sessions, Inbox, Computers, Settings. Desktop tabs: Inbox, Sessions, Usage, Settings. Check `src/words_lint.rs` and the CONTRIBUTING.md glossary.
- Amber (`--color-accent`) only for what needs you and the one primary button in a view. Working is a blue ring. No `#86a6f2` link blue, no Claude orange outside the Claude logo.
- Mockup parts the page lacks today are not built: the Composer's permission-mode chip and the "Sent from this browser" line.
- No wire-protocol change; `ios/`, `android/`, `cloud/src` (except `wrangler.toml`) untouched.
- Comments say why, in plain short sentences (AGENTS.md).

## Review Focus

1. **A notification for the session on screen.** With the new `#/s/<id>` address, `sw.js` must still skip the notice when a visible window shows that session. Test in Task 4.
2. **The iOS keyboard.** Opening the composer on an iPhone must keep the header and the newest lines in view (the `visualViewport` fit from `app.js` lines 683–730). Test in Task 6 (the hook) and by hand on a phone in Task 7.
3. **Reconnect after the screen wakes.** `visibilitychange` to visible must reconnect at once and tell the computer `{type:"visible"}`. Test in Task 3.
4. **A stale `dist/`.** A change to `src/` without a rebuild must fail CI. Test in Task 1.
5. **Computer-served mode.** Opened from the QR link (`?p=<code>`) on the LAN, with no account: it pairs, and no sign-in, Computers tab or account footer appears. Test in Task 4 and Task 10.

---

## Part 1: `refactor(phone): port the phone page to React and TanStack`

### Task 1: Build pipeline with the old page unchanged

The first task moves files and adds the build, but the built page is still today's `app.js`. Every later task can be checked against a working page.

**Files:**
- Create: `phone/package.json`, `phone/package-lock.json`, `phone/tsconfig.json`, `phone/vite.config.ts`, `phone/.gitignore` (`node_modules/`), `phone/tools/check-dist.sh`
- Move (`git mv`) to `phone/public/`: `e2e.js`, `relay.js`, `diagrams.js`, `sw.js`, `manifest.webmanifest`, `icon*.png`, `icon.svg`, `apple-touch-icon.png`, `claude.svg`, `factory.svg`, `fonts/`, and for now `app.js`, `app.css`
- Keep: `phone/index.html` (Vite entry; script tags point at the public files)
- Create: `phone/dist/` (built)
- Modify: `core/src/phone_page.rs` (`#[folder = "../phone/dist"]`), `cloud/wrangler.toml` (both `[assets] directory = "../phone/dist"`), `phone/test/*.test.mjs` (read `../public/<file>`), `tools/brand-logo.py` (write to `phone/public/`), `lefthook.yml` (`phone/*` → `phone/**/*` in the Rust and phone globs; phone hook runs `npm test`), `cubic.yaml` (wire rule covers `phone/public/*.js`, `phone/src/**/*.ts`), `.github/workflows/codeql.yml` (ignore `phone/dist`), `.github/workflows/ci.yml` (phone job below)

**Interfaces:**
- Produces: `npm run build` (writes `dist/`), `npm test` (runs `node --test "test/*.test.mjs"` then `vitest run`), `npm run dev`, `tools/check-dist.sh` (exit 1 when `dist/` differs from a fresh build).

- [ ] **Step 1: Move the files and point the tests at `public/`**, then run `cd phone && node --test "test/*.test.mjs"`. Expected: all pass, the same count as before the move.
- [ ] **Step 2: Add `package.json`, `vite.config.ts`, `tsconfig.json`.** Vite: `root: "."`, `base: "./"`, `publicDir: "public"`, `build.outDir: "dist"`, `build.target: ["safari16.4","chrome111"]`, `rollupOptions.output` with `entryFileNames: "assets/app.js"`, `chunkFileNames: "assets/[name].js"`, `assetFileNames: "assets/[name][extname]"`, `modulePreload: { polyfill: false }`. Dependencies: `react`, `react-dom`, `@tanstack/react-router`, `@tanstack/react-query`; dev: `vite`, `@vitejs/plugin-react`, `typescript`, `vitest`, `jsdom`, `@testing-library/react`, `@testing-library/user-event`, `@types/react`, `@types/react-dom`.
- [ ] **Step 3: Write `tools/check-dist.sh`:** build to a temp dir, `diff -r` against `dist/`, print the differing files and exit 1 when they differ.
- [ ] **Step 4: Run `npm run build && tools/check-dist.sh`.** Expected: exit 0. Then change one byte in `public/app.css` without building and run the check again. Expected: exit 1 naming `dist/app.css`. Undo the change.
- [ ] **Step 5: CI.** In `ci.yml`'s cloud job, the phone step becomes `npm ci && npm test && tools/check-dist.sh` in `phone/`.
- [ ] **Step 6: Check both servers serve the built page.** Run `cargo test --locked --no-default-features phone` and `(cd cloud && npm ci && npm run check)`. Then `npx wrangler dev` in `cloud/`, open `/`, and check the page loads with no 404s in the console.
- [ ] **Step 7: Commit** `build(phone): build the phone page with vite into a committed dist`.

### Task 2: Typed wrappers and the pure helpers

**Files:**
- Create: `phone/src/lib/globals.ts`, `phone/src/lib/format.ts`, `phone/src/lib/markdown.tsx`, `phone/src/lib/icons.tsx`, `phone/src/test/setup.ts`
- Test: `phone/src/lib/format.test.ts`, `phone/src/lib/markdown.test.tsx`

**Interfaces:**
- Produces:
  - `globals.ts`: `export const RELAY: RelayApi; export const E2E: E2eApi; export const DIAGRAMS: DiagramsApi`. These read `globalThis` and carry a hand-written type for each function `app.js` calls.
  - `format.ts`: `ago(unix: number, now?: number): string`, `clock(unix: number): string`, `fmtElapsed(ms: number): string`, `statusLabel(s: SessionStatus): string`, `counts(added: number, removed: number): string`. Each has the same output as the function of the same name in `public/app.js`.
  - `markdown.tsx`: `<Markdown src={string} onPic? onTable? onDiagram? />`. It renders what `md()` and `inline()` in `app.js` (lines 959–1052) render, as React elements, never as `innerHTML`.
  - `icons.tsx`: `<Icon name={IconName} size?={12|14|16|20} />`, with SVGs from `assets/icons` (1.75 stroke). `IconName` covers every key of `ICONS` in `app.js` plus `computer`, `inbox`, `message-multiple01`, `gauge`, `settings01`, `unfold-more`, `add01`, `github`, `copy01`, `file-diff`, `square`, `more-horizontal`.

- [ ] **Step 1: Write the format tests** with values taken from running the old functions in Node through `vm` (the same way `phone/test` loads scripts). Examples: `ago(now-30, now) === "now"`, `ago(now-120, now) === "2 min"`, `fmtElapsed(65_000) === "1:05"`. Take the exact strings from the old code, not from this list.
- [ ] **Step 2: Write the markdown tests:** headings, lists, fenced code with a language, inline code, links (they get `rel="noopener"` and open in a new tab), tables, and an image link. Include one test that `<script>` in the source renders as text.
- [ ] **Step 3: Run `npx vitest run src/lib`.** Expected: FAIL, modules missing.
- [ ] **Step 4: Implement the four modules.**
- [ ] **Step 5: Run `npx vitest run src/lib`.** Expected: PASS.
- [ ] **Step 6: Commit** `refactor(phone): add typed relay wrappers and helpers in TypeScript`.

### Task 3: The link store

This task moves the connection logic out of `app.js`: lines 71–340 and 673–890 (`openLocal`, `connect`, `connectRelay`, `relayHandlers`, `linked`, `lost`, `reconnectNow`, `retryNow`, the `visibilitychange` handler and `handle`).

**Files:**
- Create: `phone/src/lib/link.ts`, `phone/src/lib/model.ts` (types and the reducer)
- Test: `phone/src/lib/model.test.ts`, `phone/src/lib/link.test.ts`

**Interfaces:**
- Produces:
  - `model.ts`: `type LinkState` (every field of `state` in `app.js` lines 42–69 that comes from the computer: `connected, host, inbox, places, session, usage, pushKey, me, pcOffline`, plus `commands`, `subagents`, `changes`, `settings`, `options`). Also `reduce(s: LinkState, msg: ServerMessage): LinkState`, a pure function that covers every `case` in `handle()` (`inbox, usage, session, entries, commands, subagents, state, changes, diff, settings, options, started, error, pong`).
  - `link.ts`: `createLink(opts: { mode: "local" | "relay"; machine?: Machine }): Link`. A `Link` has `subscribe(fn): () => void`, `getState(): LinkState`, `send(msg: ClientMessage): void`, `fetch(path: string, init?: RequestInit): Promise<Response>`, `reconnectNow(): void` and `close(): void`. There is also `useLink<T>(select: (s: LinkState) => T): T`.
  - Timings copied from `app.js`: `LOCAL_PING_MS = 15_000`, `LOCAL_SILENT_MS = 40_000`, `LOCAL_OPEN_MS = 15_000`, and a retry that starts at 500 ms with the same backoff.

- [ ] **Step 1: Write reducer tests, one per message type.** For example, `reduce(empty, {type:"inbox", host:"mbp", sessions:[s1], places:[p]})` sets `host`, `inbox`, `places`, and an `entries` message adds to `session.entries` the way `handle()` does.
- [ ] **Step 2: Write link tests with a fake socket and fake timers:**
  - no pong for 40 s closes the socket and reconnects;
  - `document.visibilityState = "visible"` plus the event sends `{type:"visible", visible:true}` and calls `reconnectNow()`;
  - hidden sends `{type:"visible", visible:false}`.
- [ ] **Step 3: Run `npx vitest run src/lib/model.test.ts src/lib/link.test.ts`.** Expected: FAIL.
- [ ] **Step 4: Implement `model.ts` and `link.ts`.** Keep the "notice opened elsewhere" check (`RELAY.noticeOutcome`) in the link, as `handle()` does.
- [ ] **Step 5: Run the same tests.** Expected: PASS.
- [ ] **Step 6: Commit** `refactor(phone): move the computer link into a typed store`.

### Task 4: Router, account, and the screens before a computer

**Files:**
- Create: `phone/src/main.tsx`, `phone/src/router.tsx`, `phone/src/lib/account.ts`, `phone/src/lib/api.ts`, `phone/src/routes/{signin,computers,pair,moved,offline,fatal}.tsx`
- Modify: `phone/public/sw.js` (the "already looking" check)
- Test: `phone/src/router.test.tsx`, `phone/src/lib/account.test.ts`, `phone/test/sw.test.mjs`

**Interfaces:**
- Consumes: `RELAY` (Task 2), `createLink`, `useLink` (Task 3).
- Produces:
  - `router.tsx`: `createAppRouter(queryClient)` with hash history and these routes: `/signin, /computers, /inbox, /sessions, /s/$session, /new, /usage, /settings, /pair, /moved, /offline, /fatal`. A `beforeLoad` on the root turns `#s=<id>` into `/s/<id>`.
  - `account.ts`: `useMe()` (`RELAY.boot`), `useMachines()` (`RELAY.machines`), `useRemoveMachine()`, `useMarkNoticeRead()`, `useSignedInSessions()` (`RELAY.sessions`) and `useSignOut()`, with query keys `["me"]`, `["machines"]`, `["sessions"]`.
  - `api.ts`: `api(path: string, init?: RequestInit): Promise<Response>`, the same as `api()` in `app.js` line 132 (plain `fetch` in local mode, `link.fetch` through the relay).
  - `mode(): "account" | "local"`, the same choice `start()` in `app.js` makes. `PAIR_CODE` comes from `?p=`.

- [ ] **Step 1: Router tests:**
  - `#s=abc` lands on `/s/abc`;
  - with no token in local mode, the page lands on `/pair`;
  - in account mode, signed out, it lands on `/signin`;
  - on an old relay host (`RELAY.kindOf` says update), it lands on `/moved`.
- [ ] **Step 2: Service worker test** (`node:test` with `vm`, as the other tests do): with a visible window at `https://h/#/s/abc`, a push for session `abc` shows no notification. At `https://h/#s=abc` it also shows none. At `https://h/#/inbox` it shows one.
- [ ] **Step 3: Run `npm test`.** Expected: the new tests FAIL.
- [ ] **Step 4: Implement the router, the account hooks, `api.ts`, the six routes (copy and behavior as in `renderSignin`, `renderMachines`, `renderPair`, `renderUpdate`, `renderOffline`, `renderFatal`) and the `sw.js` match** (`#/s/<id>` or `#s=<id>`). Keep the old look: copy the classes from `public/app.css` into `src/styles/legacy.css`.
- [ ] **Step 5: Run `npm test`.** Expected: PASS.
- [ ] **Step 6: Commit** `refactor(phone): route the page with TanStack Router and Query`.

### Task 5: Inbox, Sessions, Usage, Settings and the tab bar

**Files:**
- Create: `phone/src/routes/{inbox,sessions,usage,settings}.tsx`, `phone/src/components/{TabBar,SessionRow,NoticeBanners,SetupCards}.tsx`, `phone/src/lib/push.ts`
- Test: `phone/src/routes/inbox.test.tsx`, `phone/src/routes/sessions.test.tsx`, `phone/src/lib/push.test.ts`

**Interfaces:**
- Consumes: `useLink`, the account hooks and `api`.
- Produces:
  - `needsYou(s: Session): boolean` and `asksLine(s: Session): string` in `src/lib/sessions.ts` (from `app.js` 1988–2002);
  - `subscribePush(pushKey: string): Promise<void>` and `notifyState(): "on" | "off" | "denied" | "unsupported"` in `push.ts`;
  - `<TabBar />`, with the same tabs as today (Inbox, Sessions, "+", Usage, Phone). The words change in Task 9.

- [ ] **Step 1: Tests.**
  - Inbox: sessions that need you come first, and the badge count equals that number.
  - Sessions: the search box filters by title and project; the project chip narrows the list.
  - Push: `subscribePush` calls `pushManager.subscribe` with the key, then posts it through `api("/push", …)`, the same path `app.js` uses.
- [ ] **Step 2: Run `npx vitest run src/routes src/lib/push.test.ts`.** Expected: FAIL.
- [ ] **Step 3: Implement the four routes, the components and `push.ts`.** Port them from `renderInbox`, `renderSessions`, `renderUsage`, `renderPhone`, `setupCards`, `noticeBanners` and `subscribePush`. Keep the `localStorage` keys `arakun.hideInstall`, `arakun.hideNotify` and `arakun.machine`.
- [ ] **Step 4: Run the same tests.** Expected: PASS.
- [ ] **Step 5: Commit** `refactor(phone): port the inbox, sessions, usage and settings screens`.

### Task 6: The session screen

The transcript, composer and cards come from `app.js` lines 1084–1630 and 2750–3600.

**Files:**
- Create: `phone/src/routes/session.tsx`, `phone/src/components/session/{Transcript,Entry,Composer,PermissionCard,QuestionCard,StopRow,Pending,Shots,Shortcuts,Sheet}.tsx`, `phone/src/lib/{viewport,drafts,shots}.ts`
- Test: `phone/src/components/session/PermissionCard.test.tsx`, `QuestionCard.test.tsx`, `Composer.test.tsx`, `phone/src/lib/viewport.test.ts`, `phone/src/lib/drafts.test.ts`

**Interfaces:**
- Consumes: `useLink`, `send`, `api`, `<Markdown>`, `DIAGRAMS`.
- Produces:
  - `<PermissionCard permission={Permission} onAnswer={(optionId: string) => void} />`;
  - `useHold(ms = 700, onDone)` and `useArmed(windowMs = 3000)`;
  - `useViewportFit()`, which sets `--vh` on `<html>` from `visualViewport`;
  - `keepDraft(id, text)` and `kept(key)`, with the keys `arakun.draft.<id>` and `arakun.place`;
  - `sendShot(shot, id, session)`, which sends pieces of `SHOT_PIECE = 160 * 1024` bytes.
- The transcript renders only what is near the viewport. Use the constants from `VList` (`LOAD_AHEAD = 1500`, `REST_MS = 200`, `END_SLACK = 24`) and keep the "stick to the end while new lines arrive" behavior. A library is allowed (`@tanstack/react-virtual`) if it keeps that behavior and the open `<details>` across scroll.

- [ ] **Step 1: PermissionCard tests (fake timers).**
  - A pointer down on Allow for 699 ms, then up, does not call `onAnswer`.
  - A hold of 700 ms calls it once with the allow option's id.
  - A plain click shows the notice "Hold to allow".
  - One tap on "Always allow" shows "Tap again to always allow" and does not answer. A second tap within 3000 ms answers. A second tap after 3001 ms only arms it again.
- [ ] **Step 2: QuestionCard tests.** It shows "1 of 3". Picks and typed text survive a re-render. Skip moves on. The last question's button says Answer and sends every pick.
- [ ] **Step 3: Composer and drafts tests.**
  - The draft is saved per session and comes back after a remount.
  - Stop needs a second tap within `STOP_WINDOW = 3000`.
  - A sent message shows as pending until an `entries` message has it.
- [ ] **Step 4: Viewport test.** A `visualViewport` resize to 400 px while a textarea has focus sets `--vh: 400px`. A resize during a pinch zoom (scale > 1) changes nothing.
- [ ] **Step 5: Run `npx vitest run src/components/session src/lib/viewport.test.ts src/lib/drafts.test.ts`.** Expected: FAIL.
- [ ] **Step 6: Implement the session route and its components.** Port `renderSession`, `renderPermission`, `renderQuestion`, `renderComposer`, `renderStopRow`, `renderPending`, `renderShots`, `renderCuts`, `renderSheet`, `openChanges`, `openDiff`, `openPic` and `loadOlder`.
- [ ] **Step 7: Run the same tests.** Expected: PASS.
- [ ] **Step 8: Commit** `refactor(phone): port the session screen`.

### Task 7: New session, the switch, and the docs

**Files:**
- Create: `phone/src/routes/new.tsx`, `phone/src/components/PlusMenu.tsx`
- Modify: `phone/index.html` (only `e2e.js`, `relay.js`, `diagrams.js` as plain scripts, then `src/main.tsx`; `<div id="app">`)
- Delete: `phone/public/app.js`, `phone/public/app.css`
- Modify: `README.md`, `CONTRIBUTING.md` ("Before you commit" and "Checks"), `AGENTS.md`, `cubic.yaml` (if it lists checks)
- Test: `phone/src/routes/new.test.tsx`

**Interfaces:**
- Consumes: `useLink().places`, `send({type:"start", …})` as `newSession()` sends it.

- [ ] **Step 1: New session test.** Pick a project and an agent, type, Send: it sends the same `start` message `newSession()` does. The last place is remembered in `arakun.place`.
- [ ] **Step 2: Run `npx vitest run src/routes/new.test.tsx`.** Expected: FAIL. Implement it from `renderNew`, `renderQuick`, `openPlusMenu` and `quickSession`. Run it again. Expected: PASS.
- [ ] **Step 3: Switch `index.html` to the React entry and delete the old `app.js` and `app.css`.** Run `npm test && npm run build && tools/check-dist.sh`. Expected: all pass, exit 0.
- [ ] **Step 4: Check by hand on a real phone and a desktop browser, through both servers.**
  - The Rust app: `cargo run --no-default-features`, then pair from a phone on the LAN with the QR.
  - The Worker: `wrangler dev`, then sign in.
  - On each, check these work: sign in, pick a computer, Inbox, open a session, answer a permission with a hold, Always allow with two taps, a question, send with a picture, Stop, New session, Usage, notifications on and a push that opens the session, install to the home screen, the keyboard on iOS.
  - Write down what you checked for the pull request.
- [ ] **Step 5: Docs.** README: how to build the page and that `phone/dist` is committed. CONTRIBUTING, AGENTS.md: `phone/` changed → `(cd phone && npm ci && npm test && npm run build)`.
- [ ] **Step 6: Commit** `refactor(phone): serve the React page and drop app.js`. Open pull request 1 as a draft with the `ship-pr` skill.

## Part 2: `feat(phone): apply the brand book to the phone page`

### Task 8: Tokens, colors and the sign-in page

**Files:**
- Create: `phone/src/styles/tokens.css`, `phone/src/styles/base.css`
- Delete: `phone/src/styles/legacy.css` (its rules move into the components that use them, on the tokens)
- Modify: every component's styles; `phone/src/routes/signin.tsx`; `phone/public/manifest.webmanifest` and `index.html` `theme-color` (`#0b0b0b`)
- Add: `phone/public/mascot.svg` (from https://doop.arakun.dev/a/mHmDydeYcl.svg), `phone/public/icon-small.svg` (https://doop.arakun.dev/a/GToC69rXx3.svg)
- Test: `phone/src/styles/tokens.test.ts`

**Interfaces:**
- Produces: CSS custom properties with the App UI canvas names and values (dark, and light under `@media (prefers-color-scheme: light)`): `--color-background #0b0b0b / #ffffff`, `--color-sheet #131313 / #ffffff`, `--color-raised #1a1a1a / #f9f9f9`, `--color-sunk #0e0e0e / #f5f5f5`, `--color-text #ebebeb / #121212`, `--color-text-muted`, `--color-text-subtle`, `--color-text-faint`, `--color-border-subtle`, `--color-border`, `--color-border-strong`, `--color-fill-raised`, `--color-fill-hover`, `--color-fill-selected`, `--color-success`, `--color-error`, `--color-link` (the working ring only), `--color-diff-add`, `--color-diff-del`, `--color-accent #ffb547 / #d47f00`, `--color-on-accent #0b0b0b / #121212`, `--radius-sm 4px`, `--radius-md 8px`, `--radius-lg 12px`, `--radius-xl 16px`, `--text-xs 11px`, `--text-sm 12px`, `--text-md 13px`, `--text-lg 14px`, `--text-title 20px`, `--text-m-sub 15px`, `--text-m-body 17px`, `--text-m-title 22px`, `--text-m-large 34px`, `--font-sans`, `--font-mono`. Get the exact values with `get_tokens` on canvas `sEA9hKJMVj`, format `css`, themes default and `light`.

- [ ] **Step 1: Token lint test.** Read every `.css` and `.tsx` file under `src/` except `tokens.css`, and fail on any hex color, `rgb(`/`rgba(` value, or a `border-radius` that is not a `var(--radius-*)`, 50% or 999px. This mirrors `src/design_lint.rs` for the app.
- [ ] **Step 2: Run `npx vitest run src/styles`.** Expected: FAIL, listing the hand-written values.
- [ ] **Step 3: Move every component onto the tokens.**
  - Primary buttons are amber, one per view.
  - Links take the text color, underlined.
  - The Claude orange stays only inside its logo.
  - The working state is a blue ring.
- [ ] **Step 4: Sign-in page as in the mockup "Desktop · 1440 · Sign in".**
  - The eyebrow `app.arakun.dev` in mono.
  - The title "Reach your coding agents from any screen."
  - The line "Sign in with the GitHub account you use in Arakun on your computer. Your sessions stay on that computer. This page talks to it end-to-end encrypted."
  - One amber "Sign in with GitHub" button.
  - The install line `curl -fsSL https://arakun.dev/install | sh`, with the URL in amber and a copy button.
  - The mascot, cropped at the right.
  - No second raccoon.
  - Below 768 px the mascot sits above the title at 160 px.
- [ ] **Step 5: Run `npm test && npm run build && tools/check-dist.sh`.** Expected: PASS, exit 0. Take screenshots of sign-in, Inbox and a session at 390 and 1440 wide, dark and light, for the pull request.
- [ ] **Step 6: Commit** `feat(phone): apply the brand book to the phone page`. Open pull request 2 on top of 1.

## Part 3: `feat(phone): lay out the phone page for tablet and desktop`

### Task 9: The responsive shell and navigation

**Files:**
- Create: `phone/src/components/shell/{Shell,Sidebar,Rail,TopBar,ComputerPicker}.tsx`, `phone/src/lib/useWidth.ts`
- Modify: `phone/src/components/TabBar.tsx` (phone tabs), `phone/src/router.tsx` (the root route renders `<Shell>`; `/computers` is a tab on phone only)
- Test: `phone/src/components/shell/Shell.test.tsx`

**Interfaces:**
- Produces:
  - `useWidth(): "phone" | "tablet" | "desktop"`, from `matchMedia` with the breakpoints in the Global Constraints;
  - `<Shell>`: the phone gets the routes with `<TabBar>` (Sessions, Inbox, Computers, Settings; Usage is a row in Settings); the tablet gets `<Rail>` and the outlet; the desktop gets `<Sidebar>` and the outlet;
  - `<ComputerPicker>`: the computer's name, kind and online dot. In account mode it opens a menu with the computer list, Remove and Sign out. In local mode it is a plain label.

- [ ] **Step 1: Shell tests**, with `matchMedia` stubbed per width.
  - At 390, the tab bar shows Sessions, Inbox, Computers and Settings, and a "+" sits in the top bar.
  - At 834, the rail shows one tile per live session, with a `aria-label` "<title>, needs you" or "<title>, working". The "+" tile comes after the tab icons.
  - At 1440, the sidebar shows the Inbox, Sessions, Usage and Settings tabs, a "New session" row, then the sessions grouped under "Needs you", "Working" and "Done".
  - In local mode at 1440, there is no account footer and the picker has no menu.
- [ ] **Step 2: Run `npx vitest run src/components/shell`.** Expected: FAIL.
- [ ] **Step 3: Implement it as in the mockups** "Desktop · 1440 · Session, agent asks", "Tablet · 834 portrait · Session, rail folded" and "Phone · 390 · Inbox".
  - On tablet and desktop, the session route renders in the pane next to the list. On a phone it is its own screen.
  - On desktop with no session open, the pane shows the first session that needs you, or an empty state: "Pick a session on the left."
- [ ] **Step 4: Run the same tests.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(phone): add the sidebar and rail for tablet and desktop`.

### Task 10: The session pane at each width, and the check

**Files:**
- Modify: `phone/src/routes/session.tsx`, `phone/src/components/session/*.tsx` (sizes per width), `README.md` (the Settings rename, if the README names the Phone tab)
- Test: `phone/src/routes/session.test.tsx`

- [ ] **Step 1: Session layout test.** At 1440, the transcript column is 760 px wide, centered, with the composer under it. At 834 and 390, the buttons in the permission card are at least 44 px tall.
- [ ] **Step 2: Run it.** Expected: FAIL. Implement it. Run it again. Expected: PASS.
- [ ] **Step 3: Check by hand, as in Task 7 Step 4,** at 390, 834 portrait, 1194 landscape and 1440, in dark and light, in both modes.
  - The iOS keyboard on an iPhone and on an iPad.
  - Rotating an iPad with a session open keeps the session.
  - Take screenshots of each width for the pull request.
- [ ] **Step 4: Run `npm test && npm run build && tools/check-dist.sh`, `tools/rustfmt.sh --check` and `cargo test --locked --no-default-features words_lint design_lint`.** Expected: all pass.
- [ ] **Step 5: Commit** `feat(phone): lay out the session for tablet and desktop`. Open pull request 3 on top of 2 and comment progress on #808.
