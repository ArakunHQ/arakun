# app.arakun.dev redesign

Issue: #808. Mockups: App UI canvas, page "app.arakun.dev redesign"
(https://doop.arakun.dev/c/sEA9hKJMVj?page=KECtakcOwO), approved on 2026-11-25.

## Why

The phone page in `phone/` is what `app.arakun.dev` serves, and what the app serves
itself to a paired phone. People also open it on tablets and desktop browsers, where
it is a stretched phone column. It should look like Arakun: the brand book, the
desktop app's layout on wide screens, and the native apps' look on phones. The owner
also wants it on React with TanStack Router and TanStack Query, so it is easier to
grow than 3,900 lines of string templates in `app.js`.

## Goals

- One page that adapts: phone (one column), tablet (folded rail plus session),
  desktop (sidebar plus session), as in the four mockup frames.
- Follows the brand book: black or white ground, amber only for what needs you and
  the one primary button in a view, Geist and Geist Mono, corners 4/8/12/16, one
  raccoon per screen, plain voice.
- React with TanStack Router and TanStack Query, built by Vite, in TypeScript.
- Everything the page does today still works: sign-in, the computer list, the
  Inbox, Sessions, the session view with permissions, questions and pictures, new
  sessions, Usage, the Phone tab's settings, push, install to the home screen, the
  old-host "this address has moved" page and the fatal and offline pages.

## Non-goals

- No new features beyond the layouts. ("And more" from the request was not
  specified; anything new is its own issue.)
- No change to the wire protocol, the crypto or the relay. `ios/` and `android/`
  are not touched.
- No server rendering. TanStack Start does not fit: the app serves the page as plain
  files.
- No change to the Worker's routes or the Rust server beyond the folder they serve.

## Anti-goals

- The page silently out of date: a Rust or Worker build that serves an old page.
- A port that loses a behavior people rely on: the iOS keyboard fit, reconnect
  after the screen wakes, the push deep link to a session.
- Rust builds that need Node.

## Constraints found in the code

- **Two servers.** The Worker serves `phone/` as static assets
  (`cloud/wrangler.toml`, `[assets] directory = "../phone"`, also under
  `env.production`). The app embeds it with `rust_embed`
  (`core/src/phone_page.rs`, `#[folder = "../phone"]`) and serves exact paths only,
  with `index.html` for `/` and a 404 for anything else
  (`core/src/phone/mod.rs`, around line 1520). There is no fallback to
  `index.html`, so the router uses **hash history**. The session route's address
  becomes `#/s/<session>`. `sw.js` checks `#s=<session>` in an open window's URL
  to skip a notice for the session already on screen; it changes to match the new
  address (and still accepts the old one), or notices would show for the session
  you are looking at.
- **Two modes.** With an account (app.arakun.dev) the page signs in, lists
  computers and talks through the relay. Served by the computer itself, it pairs
  with a code from the QR link (`?p=`, `PAIR_CODE` in `app.js`, `PairRequest` in
  `core/src/phone/mod.rs`) and has no account, no sign-in and no computer list.
  Both modes stay.
- **Three kinds of requests.** Worker calls (`/me`, machines, signed-in sessions,
  notices) go through functions `relay.js` already has (`boot`, `machines`,
  `sessions`, `act`). Computer calls go through `api()` in `app.js`: plain
  `fetch` on the LAN, or `link.fetch` over the relay. Live events come over the
  link.
- **The Worker's CSP** is `script-src 'self'` (`cloud/src/teams-router.ts`), so
  the build must emit no inline script.
- **The crypto is shared.** `relay.js` and `e2e.js` are plain scripts that set
  `globalThis.RELAY` and `globalThis.E2E`. `phone/test/*.test.mjs` loads them with
  `node:vm` and checks them against `phone/test/vectors`, which the Rust side
  writes and the iOS and Android tests read too. They stay plain scripts, byte for
  byte, loaded with a `<script>` tag before the React bundle. React reaches them
  through a typed wrapper (`src/lib/relay.ts`) that reads the globals.
  `diagrams.js` and its test stay the same way.
- **The service worker** (`sw.js`) caches nothing on purpose and must stay at the
  root of the served folder so its scope covers the page.
- **CI.** `ci.yml` marks `phone/*` as touching Rust, cloud, iOS and Android, and
  runs `node --test "test/*.test.mjs"` in `phone/`. The cloud hotfix workflow
  (`cloud-deploy.yml`) allows changes only under `cloud/` and `phone/`.

## Approaches considered

Where the build step lives, now that the page needs one:

- **A. Commit the built page (chosen).** Source in `phone/src`, `npm run build`
  writes `phone/dist`, committed. Cargo and the Worker serve `phone/dist`. CI
  rebuilds and fails when the committed copy differs. Cost: a bundle diff in pull
  requests that change the page. Chosen because Rust builds stay as they are and a
  stale page cannot ship silently.
- B. `build.rs` runs npm. Every Rust build on every platform would need Node.
- C. Build only in CI and releases, with a placeholder page locally. A local build
  could serve an old or empty page without saying so.

The framework: React with TanStack Router and TanStack Query, as the owner asked.
Server rendering (TanStack Start) is ruled out by the two static servers above.

## Design

### Layout of `phone/`

```
phone/
  package.json, package-lock.json, tsconfig.json, vite.config.ts
  index.html            Vite entry; loads e2e.js, relay.js, diagrams.js, then the bundle
  src/                  React source (routes, components, hooks, styles)
  public/               copied as-is into dist/: e2e.js, relay.js, diagrams.js, sw.js,
                        manifest.webmanifest, icons, fonts
  dist/                 the built page, committed; what Rust and the Worker serve
  test/                 the existing node tests, now reading public/*.js
```

- `core/src/phone_page.rs` points at `../phone/dist`. `cloud/wrangler.toml` points
  both `[assets]` blocks at `../phone/dist`.
- Vite builds with fixed file names (no content hashes) so `dist/` diffs stay
  small and stable, and `base: "./"`. Build target: Safari 16.4 and Chrome 111,
  since Safari 16.4 is the first iOS release with web push for home-screen apps.
  No legacy plugin.
- The test files read `../public/relay.js` and so on instead of `../relay.js`.

### State and data

- **TanStack Query** for request and response: the Worker calls (through the
  `relay.js` functions above: account and notices, the computer list, removing a
  computer, marking a notice read, the signed-in sessions in Settings) and the
  computer calls through a typed `api()` (this phone's record, push key, images).
- **The relay link stays outside Query.** It is one long-lived encrypted socket
  that pushes events (sessions, transcript lines, permissions, usage). A small
  store (`src/lib/link.ts`) owns it, the reconnect and the visibility handling
  from `app.js`, and exposes its state to React with `useSyncExternalStore`.
  Where a relay reply answers a request (for example usage), the store may feed it
  into the Query cache with `setQueryData`; the builder decides per message.
- No other state library.

### Routes (TanStack Router, hash history)

| Route | Today |
| --- | --- |
| `/signin` | `renderSignin` |
| `/computers` | `renderMachines` |
| `/inbox` | `renderInbox` |
| `/sessions` | `renderSessions` |
| `/s/$session` | `renderSession` with its sheets, permission and question cards |
| `/new` | `renderNew` (new session) |
| `/usage` | `renderUsage` |
| `/settings` | `renderPhone` (the Phone tab) |
| `/pair` | `renderPair` (computer-served mode) |
| `/moved`, `/offline`, `/fatal` | `renderUpdate`, `renderOffline`, `renderFatal` |

The old `#s=<session>` link keeps working: a redirect maps it to `/s/<session>`.

**Navigation words.** The tab today called "Phone" becomes **Settings** at every
width, since the page now runs on desktops too. (The desktop mockup says
"Devices"; Settings replaces it so each width uses one word.) Settings holds what
the Phone tab holds: notifications, install, sign out, the signed-in sessions.
The full computer list, with Remove, is the Computers tab on a phone and the
computer picker's menu on tablet and desktop. Check the words against the
glossary in CONTRIBUTING.md and `src/words_lint.rs`.

**New session.** The "+" in today's tab bar moves: on a phone to the top bar
(as in the iOS concept), on desktop to a "New session" row at the top of the
sidebar's sessions, on tablet to a "+" tile under the tab icons.

**Computer-served mode.** No sign-in route, no Computers tab and no account
footer. The computer picker becomes a plain label with the computer's name and
its online dot.

### Responsive layout

Three widths, from the mockups:

- **Phone, under 768 px.** One column, the bottom tab bar (Sessions, Inbox,
  Computers, Settings, as in the iOS concept), large titles, a computer picker
  chip at the top. Usage moves under Settings. The session is its own screen.
- **Tablet, 768 to 1199 px.** A 64 px rail: logo, tab icons, then one tile per
  live session (the agent's logo with a state dot), and the computer at the
  bottom. The open session fills the rest.
- **Desktop, 1200 px and up.** A 288 px sidebar: logo and name, the computer
  picker, the tabs (Inbox, Sessions, Usage, Devices), the sessions grouped as
  Needs you, Working and Done, and the account at the bottom. The session pane
  holds a 760 px column and the Composer.

Touch sizes (44 px) apply to tablet and phone. The iOS keyboard fit and the safe
areas from `app.css` carry over.

### Look

- Colors, sizes and corners come from one CSS file of custom properties that
  copies the App UI canvas tokens (`--color-background`, `--color-raised`,
  `--color-text-*`, `--color-border*`, `--radius-sm/md/lg/xl`, `--text-*`), dark
  by default with the light set under `prefers-color-scheme: light`, as today.
- The blue link color and the Claude orange go. Links are the text color,
  underlined. Agent logos keep their own color only inside the logo.
- Amber marks what needs you (dots, the Inbox count) and the one primary button
  per view. Working is a blue ring, never amber, at every width (the phone
  Inbox mockup's amber Working dot is wrong). Status colors (green, red, blue)
  mean status only and always come with a word.
- The permission card keeps its safety gestures at every width, mouse included
  (decided with the owner): Allow is "Hold to allow" with a fill, and "Always
  allow" lasts the session and needs two taps. The mockups' plain buttons show
  the look, not the behavior.
- Mockup elements that today's page lacks are dropped: the permission-mode chip
  in the Composer and the "Sent from this browser" line. They would need new
  messages on the wire.
- Screens with no mockup (phone session, New session, Usage, Settings, the list
  views at tablet and desktop width, the sheets, the light theme) are designed by
  the builder from the same canvas components and the frames that exist. Each
  pull request shows screenshots of them at the three widths for review.
- Icons are the line icons from `assets/icons`, inlined as SVG.
- The sign-in page shows the mascot, large, and no second raccoon.
- Plain CSS (CSS modules or one stylesheet per component). No CSS framework.

### Pull requests (a stack)

1. `refactor(phone): port the phone page to React and TanStack` — same look and
   behavior, the new build, `dist/` committed, the CI check that `dist/` is up to
   date, Rust and the Worker pointed at `dist/`.
2. `feat(phone): apply the brand book to the phone page`.
3. `feat(phone): lay out the phone page for tablet and desktop`.

Each pull request says how it was tested on a phone and a desktop browser.

### Docs and CONTRIBUTING

- README: how to build the page (`cd phone && npm ci && npm run build`) and that
  `phone/dist` is committed.
- CONTRIBUTING "Before you commit": `phone/` changed → `(cd phone && npm ci &&
  npm run build && npm test)`; also in AGENTS.md and `cubic.yaml` if it lists checks.
- `tools/setup-dev.sh` and `.ps1`: Node is already needed for `cloud/`; check that
  they install it.
- Other places that read `phone/` paths move with it:
  - `tools/brand-logo.py` writes the icons to `phone/public/`.
  - `lefthook.yml`: the Rust and phone globs `phone/*` become `phone/**/*` so
    nested files fire the hooks.
  - `cubic.yaml`: the wire-format rule covers `phone/public/*.js` and
    `phone/src/**/*.ts`.
  - `codeql.yml` skips `phone/dist/`.

## Left to the builder

- Component split, file layout under `src/`, and test tooling for React code
  (Vitest is already used in `cloud/`).
- The exact way the CI job checks `dist/` (rebuild and `git diff --exit-code`).
- Which relay replies feed the Query cache.
- Whether `diagrams.js` becomes a React component in pull request 1 or stays a
  script it calls.
