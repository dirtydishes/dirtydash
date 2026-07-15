# dirtydash deployment

## Signed Hub deployment (fleet)

The fleet deployment path is separate from the historical Docker/Nginx helper below. Inspect a non-mutating plan first:

```bash
dirtydash deploy hub <ssh-target> --plan --json
```

Apply only a verified release with an externally managed Ed25519 public key:

```bash
dirtydash deploy hub <ssh-target> --apply \
  --manifest release/manifest.json \
  --artifact-dir release/artifacts \
  --public-key release/signing-public-key
```

Tailscale Serve is the default private listener. Use `--listener public` only with the explicitly configured fallback administrator/trusted-proxy policy. The command never accepts passwords or private keys as arguments or environment variables; SSH aliases and the host's configured key agent handle transport authentication. Production signing keys and live host/Tailscale consent are external release evidence.

The installer uses versioned user-owned paths, non-root systemd user or launchd services, atomic activation, health verification, and rollback/cleanup. An optional `--db-seed PATH` is transferred through SSH stdin and is not serialized into the plan.

dirtydash is deployed on `di` behind Nginx Proxy Manager at:

```text
https://dirtydash.dirtydishes.dev
```

The public route is protected by NPM Basic Auth. The generated password is not stored in this repository.

## Current server layout

```text
/home/delta/apps/dirtydash/app                  # git checkout
/home/delta/apps/dirtydash/data                 # mounted container data directory
/home/delta/apps/dirtydash/data/dirtydash.sqlite3
```

Runtime shape:

```text
Docker container: dirtydash
Docker network: npm-shared
NPM upstream: http://dirtydash:4599
NPM proxy host: dirtydash.dirtydishes.dev
```

The container mounts the compiled release binary from the server checkout:

```text
/home/delta/apps/dirtydash/app/target/release/dirtydash
```

## Deploy from your local machine

From the repository root:

```bash
scripts/deploy-dirtydash
```

The script asks where to deploy:

```text
Deploy dirtydash where? [R]emote via ssh di / [l]ocal server shell:
```

Press Enter for the default remote deploy to `di`. Remote mode SSHes to `di`, pulls `main`, rebuilds dashboard assets, rebuilds the Rust release binary, recreates the private `dirtydash` container, and smoke-tests the NPM upstream.

Run remote mode without the prompt, useful for automation:

```bash
scripts/deploy-dirtydash --remote-mode
```

Deploy a different branch:

```bash
scripts/deploy-dirtydash --branch lavender/some-branch
```

Deploy the current server checkout without pulling:

```bash
scripts/deploy-dirtydash --skip-pull
```

Use a different SSH target:

```bash
scripts/deploy-dirtydash --remote di
```

## Deploy directly on the server

SSH to `di`, then run:

```bash
cd /home/delta/apps/dirtydash/app
scripts/deploy-dirtydash
```

Answer `local` at the prompt. You can also skip the prompt:

```bash
scripts/deploy-dirtydash --local
```

If you already pulled the exact revision you want:

```bash
scripts/deploy-dirtydash --local --skip-pull
```

## Manual SQLite sync

The deployed dashboard reads the server-side SQLite file:

```text
/home/delta/apps/dirtydash/data/dirtydash.sqlite3
```

Use the local helper created during deployment to replace that file with a consistent backup copy from this Mac:

```bash
dirtydash-sync-db
```

The sync helper is intentionally outside the repository because it is machine-local and knows this Mac's local dirtydash data path.

## Validation

Useful checks:

```bash
ssh di 'docker ps --filter name=dirtydash'
ssh di 'docker logs --tail 50 dirtydash'
ssh di 'docker exec dirtydash dirtydash --db /data/dirtydash.sqlite3 doctor --json'
ssh di 'docker exec nginx-proxy-manager curl -fsS http://dirtydash:4599/api/summary >/dev/null'
```

Check the public route with Basic Auth:

```bash
curl -u 'kell:<password>' https://dirtydash.dirtydishes.dev/api/summary
```

Unauthenticated browser requests should get `401 Authorization Required`.

## Configuration

The deploy script supports these environment overrides:

```text
DIRTYDASH_DEPLOY_REMOTE       # default: di
DIRTYDASH_DEPLOY_APP_DIR      # default: /home/delta/apps/dirtydash/app
DIRTYDASH_DEPLOY_DATA_DIR     # default: /home/delta/apps/dirtydash/data
DIRTYDASH_DEPLOY_CONTAINER    # default: dirtydash
DIRTYDASH_DEPLOY_NETWORK      # default: npm-shared
DIRTYDASH_DEPLOY_IMAGE        # default: debian:trixie-slim
DIRTYDASH_DEPLOY_PORT         # default: 4599
DIRTYDASH_DEPLOY_BRANCH       # default: main
```

## Rollback

To stop the deployed container:

```bash
ssh di 'docker rm -f dirtydash'
```

To redeploy the last known-good commit, check it out on the server and run:

```bash
ssh di 'cd /home/delta/apps/dirtydash/app && git checkout <commit> && scripts/deploy-dirtydash --local --skip-pull'
```

NPM route removal should be done in Nginx Proxy Manager by deleting proxy host `dirtydash.dirtydishes.dev`. Remove the related access list only after the proxy host is gone.
