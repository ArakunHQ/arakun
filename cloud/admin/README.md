# Admin panel

The operator panel for Arakun's cloud: a second Cloudflare Worker behind Cloudflare
Access. It checks the operator's Access token on every request, serves a React page
(`src/`, built by Vite) and forwards `/api/*` to the cloud Worker's `/admin/*` routes
over a service binding (`worker/`).

```sh
npm ci
npm run check    # tsc --noEmit
npm test         # vitest run
npm run build    # vite build, into dist/client
npm run dev      # vite dev; needs the cloud Worker running too
```

A production build needs `CLOUDFLARE_ENV=production`, or it is the dev Worker's. `npm run
deploy` sets it.

Running both Workers locally, the production setup (the Access application and the
secret) and how to add or remove an operator are in "The admin panel" in
[../README.md](../README.md).
