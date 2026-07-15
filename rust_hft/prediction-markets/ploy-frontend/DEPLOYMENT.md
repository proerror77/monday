# PLOY frontend delivery inside Monday

The PLOY frontend is maintained at `rust_hft/prediction-markets/ploy-frontend` in the Monday
monorepo. It is not delivered from a separate repository and this migration does not
authorize a production Vercel or host deployment.

## Local validation

Run from `rust_hft/prediction-markets`:

```bash
npm --prefix ploy-frontend ci
npm --prefix ploy-frontend run contracts:check
npm --prefix ploy-frontend run lint
npm --prefix ploy-frontend run build
npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend
```

The production build output is `ploy-frontend/dist`. The root PLOY CI workflow also
checks generated operator contracts and rejects JavaScript chunks larger than 500 KiB.

## Deployment boundary

Any future frontend deployment requires a Monday-owned reviewed change that defines
the target, authentication, secrets, origin/API routing, immutable artifact identity,
and rollback. Nested PLOY workflows and former standalone frontend instructions are
historical only.
