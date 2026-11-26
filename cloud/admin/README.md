# Admin panel

The operator panel for Arakun's cloud: a second Cloudflare Worker that signs operators
in with GitHub and a code from an authenticator app, serves a React page (`src/`, built by Vite) and forwards `/api/*` to the
cloud Worker's `/admin/*` routes over a service binding (`worker/`).

```sh
npm ci
npm run check    # tsc --noEmit
npm test         # vitest run
npm run build    # vite build, into dist/client
npm run dev      # vite dev; needs the cloud Worker running too
```

A production build needs `CLOUDFLARE_ENV=production`, or it is the dev Worker's. `npm run
deploy` sets it.

Running both Workers locally, the production setup (the secrets for arakun.dev, an
OAuth App for a copy of the repository), how sign-in works and how to add or remove an
operator or their authenticator key are in "The admin panel" in
[../README.md](../README.md).
