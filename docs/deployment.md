# dirtydash deployment

## Signed Hub deployment (fleet)

The fleet deployment path is separate from the historical Docker/Nginx helper below. Configure the durable publisher trust anchor before running deployment; flags are optional assertions and cannot establish or replace trust:

```toml
[hub]
allowed_publisher_key_id = "release-key-2026"
allowed_publisher_fingerprint = "sha256:<64 lowercase hexadecimal characters>"
```

If either value is absent, deployment fails closed. The manifest and public-key files are release evidence only; replacing either file, or supplying replacement publisher flags, cannot authorize a different publisher.

Run the concrete read-only probe first. It resolves the target with `ssh -G`, observes the effective host key, verifies the pinned publisher, probes target facts, and persists a secret-free plan/checkpoint:

```bash
dirtydash deploy hub <ssh-target> --plan --json \
  --manifest release/manifest.json \
  --artifact-dir release/artifacts \
  --public-key release/signing-public-key \
  --publisher-key-id <allowed-key-id> \
  --publisher-fingerprint <allowed-fingerprint> \
  --confirm-host-fingerprint <observed-sha256>
```

Review the printed `plan_hash`, then apply only that persisted plan:

```bash
dirtydash deploy hub <ssh-target> --apply \
  --manifest release/manifest.json \
  --artifact-dir release/artifacts \
  --public-key release/signing-public-key \
  --publisher-key-id <allowed-key-id> \
  --publisher-fingerprint <allowed-fingerprint> \
  --approved-plan-hash <reviewed-plan-hash> \
  --confirm-host-fingerprint <observed-sha256>
```

The publisher ID/fingerprint flags are optional assertions of the non-secret `[hub]` anchor; they can never replace it. Unknown managed host keys require the exact explicit confirmation; changed keys are blocked. Non-default SSH ports are passed as `-p <port>`, never as `user@host:port`.

Tailscale Serve is the default private listener and the Hub binds loopback-only in that mode. Use `--listener public` only with the explicitly configured fallback administrator/trusted-proxy policy and valid source CIDRs. The command never accepts passwords or private keys as arguments or environment variables; production signing keys, live hosts, system managers, certificates, and tailnet consent remain external release evidence. Password enrollment uses a controlled live PTY: password and sudo bytes are bounded, zeroized, written only after fixed prompts, and never enter argv, environment, temp/state files, logs, or transcripts. SQLite seeds are header-validated locally and again remotely with Python/`od` byte comparison, including WAL sidecars, without putting NUL bytes in shell variables.

The installer snapshots actual prior listener/service/current-pointer state, quiesces both user services, validates a SQLite backup including WAL/SHM, activates with a platform-specific atomic rename, independently health-checks Hub and Collector, and restores all captured state on rollback. Rollback restarts both prior services, checks the old Hub `/healthz`, and runs old-Collector diagnostics; a rollback or rollback-health failure is an explicit manual-recovery blocker and retains its snapshot. An optional `--db-seed PATH` is transferred through SSH stdin and its digest/size/backfill intent are bound into the reviewed plan.

Bootstrap and Collector tokens are loaded from the adjacent restrictive `secrets.json` (atomic mode `0600`) and are never serialized into `config.toml`.

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
