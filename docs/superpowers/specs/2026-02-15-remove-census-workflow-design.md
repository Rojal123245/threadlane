# Remove Census GitHub Workflow

## Goal

Stop running the Panic Construct Census GitHub workflow while preserving the existing census script for possible local or future use.

## Scope

- Delete `.github/workflows/census.yml`.
- Leave `scripts/unwrap_census.sh` unchanged.
- Do not modify other workflows or source files.

## Verification

- Confirm the census workflow file is removed.
- Confirm `scripts/unwrap_census.sh` remains present and unchanged.
- Run `git diff --check`.
