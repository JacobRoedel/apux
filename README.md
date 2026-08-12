# Apux

Apux is a preflight tool for unattended commands. It exposes the hidden execution
context that makes a command work in a developer terminal but fail under cron.

> Early MVP: cron is the first supported target. systemd, launchd, Docker, and
> CI adapters are planned.

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
```

`check` is intentionally conservative: warnings are hypotheses to inspect, not
claims that every job will fail. `run` is the proof step; it starts the command
with an empty environment and adds only cron's normal shell, PATH, and HOME.

## What it checks today

- command and script existence/executable permissions
- missing shebangs and Bash syntax that `/bin/sh` may reject
- shell startup files and interactive-only assumptions
- variables referenced by the script which cron will not inherit
- relative paths, `~`, and working-directory assumptions
- commands found locally but absent from cron's standard PATH
- absent logging/redirection

Apux never prints environment-variable values in `diff`, and it does not read
or store secrets.

## Generated files

`apux init` writes `apux.yaml` and `.apux/run.sh`. The wrapper makes the working
directory, shell, PATH, and logging destination explicit. Review it, change the
project-specific values, then add the printed line to your crontab yourself.

## Development

```sh
cargo test
cargo run -- check --target cron -- ./example.sh
```
