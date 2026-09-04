# Contributing

aiterm is a one-person project with an open door. Read [LICENSE](LICENSE) first: you may build, run and modify it for your own use; you may not redistribute it. Pull requests are welcome under those terms.

## Branches

| Branch | Purpose |
|---|---|
| `main` | stable; every commit is releasable |
| `5lime-dev` | integration; daily work lands here |
| `feat/<slug>` | one feature or fix each; opened from `5lime-dev`, merged back with `--no-ff` |
| `release/X.Y` | created only when a beta needs fixes before `main` |

## Working on something

```bash
git checkout 5lime-dev && git pull
git checkout -b feat/pdf-thumbnails
# small commits, each message says what the user gets, not what the code does
git push -u origin feat/pdf-thumbnails      # CI builds it
```

Open a PR into `5lime-dev`. CI runs `tsc`, the UI tests, and `cargo test --lib -- --test-threads=1` (some tests set `HOME`; they must run serially).

Commit messages: one sentence in plain language, present tense, from the user's side. "A PDF opens like any other file" beats "add PdfView component".

## Releases are tags

```bash
scripts/nightly.sh                  # tonight: nightly-YYYYMMDD from 5lime-dev
scripts/release.sh alpha 0.11.0     # v0.11.0-alpha.N from 5lime-dev
scripts/release.sh beta  0.11.0     # v0.11.0-beta.N  from release/0.11
scripts/release.sh final 0.11.0     # v0.11.0 on main, marked latest
```

The `release` workflow builds AppImage/deb/rpm + the phone APK and publishes the GitHub Release. Nightlies, alphas and betas are pre-releases; only `vX.Y.Z` becomes *latest*.

## Adding an engine

Implement the adapter contract in [HARNESS-CONTRACT.md](docs/architecture/HARNESS-CONTRACT.md): parsers are pure functions with verbatim on-disk fixtures, stamped with the CLI version they were written against.
