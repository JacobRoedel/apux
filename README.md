# Apux

Apux is a preflight tool for unattended commands. It exposes the hidden execution
context that makes a command work in a developer terminal but fail under cron.

> Apux currently supports cron, systemd, launchd, and Docker preflight checks.
> GitHub Actions workflow inspection is available; interactive CI replay is next.

## Install from source

```sh
cargo install --path .
```

## Try it

```sh
# Check a job against cron's minimal environment.
apux check --target cron -- ./scripts/nightly-sync.sh

# See the environment differences that matter.
apux diff local cron -- ./scripts/nightly-sync.sh

# Run once under a clean cron-like environment.
apux run --target cron -- ./scripts/nightly-sync.sh

# Generate an explicit wrapper and starter contract (does not install a crontab).
apux init --target cron -- ./scripts/nightly-sync.sh

# Generate native, reviewable scheduler artifacts. Nothing is installed.
apux init --target systemd -- /usr/local/bin/nightly-sync
apux init --target launchd -- /usr/local/bin/nightly-sync
apux init --target docker --image node:22 -- npm run nightly

# Verify the declared command, working directory, shell, paths, and required
# environment *names* in apux.yaml.
apux verify --target cron

# Execute the declared command in its declared target context.
apux run --contract apux.yaml

# Capture a successful (or failed) run's non-secret context, then detect drift.
apux record --contract apux.yaml
apux diff --last-run --contract apux.yaml

# Open a real shell with cron's isolated environment.
apux debug --target cron -- ./scripts/nightly-sync.sh

# systemd has different rules: no implicit shell and an absolute ExecStart path.
apux check --target systemd -- /usr/local/bin/nightly-sync

# macOS launchd uses a plist and runs ProgramArguments without a shell.
apux check --target launchd -- /usr/local/bin/nightly-sync

# Docker mounts your current checkout at /workspace in the specified image.
apux check --target docker --image node:22 -- npm test
apux run --target docker --image node:22 -- npm test
apux debug --target docker --image node:22 -- npm test

# See a job's container, declared environment names, and run steps before push.
apux inspect github-actions --file .github/workflows/ci.yml --job test

# Replay all `run:` steps, or only a selected workflow step, in its job container.
apux gha run --workflow .github/workflows/ci.yml --job test
apux gha step --workflow .github/workflows/ci.yml --job test --at 4
```

`check` is intentionally conservative: warnings are hypotheses to inspect, not
claims that every job will fail. `run` is the proof step; it starts the command
with an empty environment and adds only cron's normal shell, PATH, and HOME.
`verify` makes that context review repeatable in CI or pre-commit hooks.

`run --contract` makes `apux.yaml` the source of truth. It executes cron
contracts with only their declared PATH and required environment names, and
Docker contracts in their declared image. Values are read only at runtime and
never written to the contract or diagnostic output.

`record` writes `.apux/last-run.yaml` with non-secret runtime evidence: target,
command, working directory, declared PATH/shell/image, host, user, required
environment availability, and exit code. `diff --last-run` compares that baseline
to the current contract and environment without reading environment values.

`debug` opens a shell in the cron or Docker context. It does not modify a
scheduler, install a service, or inject undeclared secrets.

## What it checks today

- command and script existence/executable permissions
- missing shebangs and Bash syntax that `/bin/sh` may reject
- shell startup files and interactive-only assumptions
- variables referenced by the script which cron will not inherit
- relative paths, `~`, and working-directory assumptions
- commands found locally but absent from cron's standard PATH
- absent logging/redirection
- systemd `ExecStart` requirements, direct-execution behavior, and journal logs
- launchd plist, direct-execution, and explicit logging requirements
- Docker image, container filesystem, working-directory, and environment drift
- GitHub Actions job container, declared environment names, and runnable steps

Apux uses shell-aware lexical analysis for common scripts: it ignores comments
and single-quoted literals and does not report variables assigned inside the
script as missing inherited environment. It does not evaluate `source`, command
substitutions, or arbitrary shell code.

GitHub Actions replay currently supports jobs with an explicit `container:`. It
mounts the checkout at `/github/workspace`, runs each `run:` step with
`/bin/sh -e -c`, and intentionally skips `uses:` actions; this makes the local
boundary explicit rather than pretending to emulate GitHub-hosted runners.

Apux never prints environment-variable values in `diff`, and it does not read
or store secrets.

## Generated files

`apux init` writes `apux.yaml` plus a target-native artifact in `.apux/`: a cron
wrapper, systemd unit, launchd plist, or Docker Compose file. Review it, change
project-specific values, then install or run it yourself. Apux never changes a
scheduler automatically.

The contract intentionally records only required environment-variable names in
`required_env`; never put secret values in `apux.yaml`.

## Development

```sh
cargo test
cargo run -- check --target cron -- ./example.sh
```
