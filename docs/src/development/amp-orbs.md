---
title: Developing Mutex in Amp Orbs
description: "Bootstrap, run, test, and inspect Mutex in an Amp orb."
---

# Developing Mutex in Amp Orbs

Amp orbs run Debian 12. Mutex is a native GPUI desktop application, not a web
application. The orb workflow runs the real Linux editor in Xvfb and exposes the
virtual desktop through an authenticated noVNC portal.

The marketing site is a separate repository and Amp project:
`jamesnicolas/mutex.so` and `jenicola/mutex.so`. Do not clone or deploy it from
this repository.

## Bootstrap {#amp-orb-bootstrap}

The committed lifecycle files are:

- `.agents/setup`: installs the Debian build and virtual-display packages,
  fetches locked Cargo dependencies, and starts declared orb services.
- `.agents/resume`: recreates runtime directories and reconciles declared
  services when an orb wakes.
- `.amp/services.yaml`: runs the native editor in a supervised virtual desktop.
- `.amp/environment.yaml`: lists environment and credential names without
  secret values.

Both lifecycle hooks are executable and idempotent. To repair a running orb:

```sh
./script/amp bootstrap
amp orb service ensure --json
./script/amp doctor
```

Amp writes automatic hook output to
`/home/user/.cache/amp/logs/setup.log` and
`/home/user/.cache/amp/logs/resume.log` inside the orb.

`./script/amp bootstrap` uses the repository's `script/linux`, installs the
additional Xvfb, software Vulkan, noVNC, and screenshot packages, then runs
`cargo fetch --locked`.

## Run and inspect Mutex {#amp-orb-run}

Start or reconcile the supervised desktop:

```sh
amp orb service ensure --json
amp orb service status desktop
amp orb service logs desktop -n 100
```

The service opens an authenticated portal named **Mutex**. It builds
`crates/zed`, starts the real `target/debug/mutex` binary with an isolated user
data directory, and opens the repository root. The portal is private to people
who can view the Amp thread.

Create acceptance evidence after the window appears:

```sh
./script/amp screenshot
```

The command waits for a visible Mutex window, captures it to
`.amp/in/artifacts/mutex-orb.png`, and prints the image dimensions, SHA-256, and
running process. `.amp/in/` is gitignored.

For a local desktop session, run:

```sh
./script/amp run
```

## Check and test {#amp-orb-test}

Use the locked dependency graph:

```sh
./script/amp check
./script/amp test
```

Pass an alternative Cargo test selection after `--`:

```sh
./script/amp test -- -p gpui --lib
```

The default `a1.small` orb limits the build to two concurrent Cargo jobs. A
clean editor build can take several minutes. Do not increase the project orb
size without approval.

## Credentials {#amp-orb-credentials}

Building, testing, running, and screenshotting the editor require no secrets.
Verify the project state without exposing values:

```sh
amp secrets list --project jenicola/mutex --json
amp secrets history --project jenicola/mutex --json
```

When a future development-only integration requires a value, store it directly
in Amp. Do not put the value in a command argument, shell history, repository
file, thread message, or log:

```sh
read -rsp 'Secret value: ' AMP_SECRET_VALUE && echo
printf %s "$AMP_SECRET_VALUE" |
  amp secrets set NAME \
    --project jenicola/mutex \
    --secret \
    --data-file -
unset AMP_SECRET_VALUE
```

Use `--env` instead of `--secret` only for a non-sensitive value. Personal
entries override project entries, and project entries override workspace
entries.

Prefer short-lived workload identity when a provider supports it:

```sh
amp orb id-token --audience AUDIENCE
```

Configure the provider's trust policy before using this command. Treat the
minted token as a secret: pass it directly to the relying process and never
print or persist it.

Production release, documentation, collaboration, signing, and cloud
credentials stay in their protected GitHub environments. Do not copy them into
Amp to make local development work. The development-only placeholders in
`crates/collab/.env.toml` are not production credentials.

## Deployment topology {#amp-orb-deployment-topology}

Run `./script/amp topology` for a concise topology and mutation boundary.

- The desktop product is built and tested from this repository. Protected tag
  workflows build platform bundles and create GitHub releases.
- The optional local collaboration stack uses PostgreSQL, MinIO, LiveKit, and
  `crates/collab`. It is not required for the native editor smoke test.
- The inherited collaboration deployment publishes a container and rolls it
  out to DigitalOcean Kubernetes. It is currently gated to upstream repository
  owners.
- The inherited documentation deployment builds mdBook output and sends it to
  Cloudflare. It is currently gated to upstream repository owners.
- `mutex.so` is the separate marketing-site repository and deployment.
- The Amp project has a deployment resource configured, but this native app is
  not an Amp web deployment. Do not run `amp projects deploy` here.

Inspect state with read-only commands:

```sh
./script/amp operator-status
amp projects status --json
gh run list --repo jamesnicolas/mutex --limit 10
```

Do not create or rotate credentials, change access policy, resize the orb,
dispatch a deployment, create a release tag, publish a release, or deploy the
separate website without explicit approval.

## Portable workflow mapping {#amp-orb-portable-workflows}

Use repository-owned commands instead of workstation assumptions:

| Previous assumption                | Portable Amp equivalent                                                       |
| ---------------------------------- | ----------------------------------------------------------------------------- |
| A Codex-specific background thread | Any Amp orb thread scoped to `jenicola/mutex`                                 |
| A path under `/Users/nicolas`      | The orb repository root discovered by `script/amp`                            |
| Homebrew or an existing macOS SDK  | `.agents/setup` on Debian 12                                                  |
| A logged-in Mac desktop            | The private `desktop` noVNC portal                                            |
| A local screenshot tool            | `./script/amp screenshot`                                                     |
| Forwarded workstation credentials  | Amp secret names or short-lived OIDC                                          |
| A local deployment alias           | Read-only `./script/amp operator-status`, then an approved protected workflow |

Platform-specific macOS, Windows, and Linux code remains part of the product.
The portable workflow removes workstation coupling; it does not remove
supported platforms or intentional Codex integration features from Mutex.
