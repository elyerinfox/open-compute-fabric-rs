# Open Compute Fabric — Web Frontend

A responsive, dark-themed control plane for **Open Compute Fabric (OCF)**, built
with **Nuxt 3 + Vite + Vue 3 + Tailwind CSS**.

It talks to the `ocf-api` REST surface, and **gracefully falls back to bundled
mock fixtures** when the backend is unreachable — so you can run and explore the
entire UI with no Rust services running.

## Quick start

```bash
cd web
npm install
npm run dev
```

Then open <http://localhost:3000>.

With no backend running, every page renders against bundled mock data and shows
a small amber **"mock data"** badge. Start `ocfd` (which serves `ocf-api` on
`:8080`) and the badge turns green **"live"**.

### Scripts

| Command             | What it does                                  |
| ------------------- | --------------------------------------------- |
| `npm run dev`       | Start the Vite dev server (HMR) on `:3000`    |
| `npm run build`     | Production build (`.output/`)                 |
| `npm run preview`   | Preview the production build                  |
| `npm run generate`  | Static prerender (SSG)                        |
| `npm run typecheck` | `vue-tsc` type check                          |

## API integration

The frontend targets these endpoints (implemented by `ocf-api`):

| Method | Path                       | Composable method        |
| ------ | -------------------------- | ------------------------ |
| GET    | `/api/v1/health`           | `getHealth()`            |
| GET    | `/api/v1/topology/tree`    | `getTopologyTree()`      |
| GET    | `/api/v1/workloads`        | `listWorkloads()`        |
| GET    | `/api/v1/networks/vpcs`    | `listVpcs()`             |
| GET    | `/api/v1/networks/subnets` | `listSubnets()`          |
| GET    | `/api/v1/loadbalancers`    | `listLoadBalancers()`    |
| GET    | `/api/v1/disks`            | `listDisks()`            |
| GET    | `/api/v1/metrics/host`     | `getHostMetrics()`       |
| GET    | `/api/v1/access/users`     | `listUsers()`            |
| GET    | `/api/v1/access/roles`     | `listRoles()`            |
| POST   | `/api/v1/workloads/:id/migrate` | `migrateWorkload(id)` (stub) |

### Base URL & proxy

The API base URL defaults to `http://localhost:8080/api/v1` and is configured
through Nuxt `runtimeConfig.public.apiBase`. Override it at runtime without
rebuilding:

```bash
NUXT_PUBLIC_API_BASE=https://fabric.internal/api/v1 npm run dev
```

In development, `nuxt.config.ts` also sets up a **dev proxy** so requests to
`/api/**` are forwarded to `http://localhost:8080/api/**` (avoids CORS while the
Vite dev server runs on a different port).

When the backend ships the built frontend, `ocf-api` serves the static assets
directly and same-origin requests need no proxy.

## Project layout

```
web/
├── app.vue                     # Root — mounts the layout + page
├── nuxt.config.ts              # Nuxt config (Tailwind module, runtimeConfig, dev proxy)
├── tailwind.config.js          # Dark theme, surface/brand palette
├── tsconfig.json
├── package.json
├── assets/
│   └── css/tailwind.css        # Tailwind entry + base/component styles
├── layouts/
│   └── default.vue             # Sidebar nav + responsive shell
├── components/
│   ├── StatCard.vue            # Summary metric card
│   ├── HealthBadge.vue         # Status pill (Health / state / disk health)
│   ├── ResourceTable.vue       # Generic styled table with per-column slots
│   ├── TreeNode.vue            # Recursive topology drill-down row
│   └── PageHeader.vue          # Page title + live/mock badge
├── composables/
│   ├── useApi.ts               # Typed fetch client w/ mock fallback
│   ├── types.ts                # TS types loosely mirroring the Rust models
│   ├── mockData.ts             # Bundled mock fixtures
│   └── useFormat.ts            # bytes / bps / millicores / date helpers
└── pages/
    ├── index.vue               # Dashboard — fleet summary cards
    ├── topology.vue            # Drill-down tree + machine detail panel
    ├── workloads.vue           # Workload table + migrate action
    ├── networking.vue          # VPCs + subnets
    ├── loadbalancers.vue       # Load balancers
    ├── storage.vue             # Physical disks
    └── access.vue              # Users / roles (RBAC)
```

## Design notes

- **Dark theme by default** (`dark` class on `<html>`), `surface-*` and
  `brand-*` palettes defined in `tailwind.config.js`.
- **Responsive**: the sidebar collapses behind a hamburger below the `lg`
  breakpoint; tables scroll horizontally; cards reflow into one column.
- **Mock-first**: `useApi()` never throws on a dead backend — it returns
  `{ data, source }` and the UI surfaces whether data is `live` or `mock`.
- **Types are loose** on purpose: they follow the JSON the API is expected to
  emit (serde defaults), not the exact Rust generics.
