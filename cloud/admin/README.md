# Admin panel

The operator panel for Arakun's cloud: a second Cloudflare Worker that signs operators
in with GitHub, serves a React page (`src/`, built by Vite) and forwards `/api/*` to the
cloud Worker's `/admin/*` routes over a service binding (`worker/`).

```sh
npm ci
npm run check    # tsc --noEmit
npm test         # vitest run
npm run build    # vite build, into dist/client
npm run dev      # vite dev; needs the cloud Worker running too
```

Running both Workers locally, the production setup (OAuth App, secrets) and how to add
or remove an operator are in "The admin panel" in [../README.md](../README.md).
