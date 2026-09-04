# Contributing

aiterm is a one-person project with an open door. Read [LICENSE](LICENSE) first: you may build, run and modify it for your own use; you may not redistribute it. Pull requests are welcome under those terms.

## How work lands

Commit straight onto `5lime-dev`, one commit per feature or fix, and push. CI builds every push. That is the whole workflow for day-to-day work; there is no branch per feature.

```bash
git checkout 5lime-dev && git pull
# build the thing
git commit -am "A PDF opens like any other file"    # what the user gets, not what the code does
git push
```

A branch exists for exactly two reasons:

- **A multi-day change** you want kept out of tonight's nightly: `git checkout -b <slug>`, merge back with `--no-ff` when it's done, delete the branch.
- **An outside contribution**: fork, branch, open a PR into `5lime-dev`. Merged PR branches delete themselves.

| Branch | Purpose |
|---|---|
| `5lime-dev` | where everything happens |
| `main` | stable; moves only when `scripts/release.sh final` cuts a version |
| `release/X.Y` | created only when a beta needs fixes before `main` |

CI runs `tsc`, the UI tests, and `cargo test --lib -- --test-threads=1` on every push (some tests set `HOME`; they must run serially).

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
