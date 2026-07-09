# Releasing tellur

Lockstep releases for all workspace crates, driven by [Knope](https://knope.tech) changesets and GitHub Actions.

## Document a change

Any PR that should appear in the next release notes must include a changeset under `.changeset/`.

Generate one interactively:

```bash
knope document-change
```

Or hand-write a file such as `.changeset/my-change.md`:

```markdown
---
default: patch
---

# Short summary

Optional longer Markdown description for the changelog.
```

- The package name in the front matter is always `default` (knope's name for the single anonymous `[package]` that covers the whole workspace in lockstep).
- `default` is the SemVer bump: `major`, `minor`, or `patch`. It also determines the changelog category:
  - `major` → **Breaking Changes**
  - `minor` → **Features**
  - `patch` → **Fixes**
- Do not put other Markdown files in `.changeset/` — knope treats every `*.md` there as a change file.

Validate pending changesets locally:

```bash
nix develop --command cargo run --manifest-path tools/release-notes/Cargo.toml -- check
```

## Cut a release

1. Merge feature PRs (with changesets) into `main`.
2. Run **Prepare Release** via `workflow_dispatch` (Actions → Prepare Release → Run workflow).
3. Review the opened `chore: release vX.Y.Z` PR (version bumps, `CHANGELOG.md`, publish dry-run CI).
4. Merge the Release PR. That tags `vX.Y.Z`, creates the GitHub Release, and publishes all crates to crates.io via Trusted Publishing.

### Local dry-run (read-only)

Preview the next changelog entry and version bump without modifying any files:

```bash
nix develop --command knope prepare-release-dry-run
```

Or call the helper directly:

```bash
nix develop --command cargo run --manifest-path tools/release-notes/Cargo.toml -- preview
```

This prints the SemVer bump implied by pending changesets and the changelog sections that would be written (derived from each changeset's `default` bump). It does not touch `CHANGELOG.md`, `Cargo.toml`, or `.changeset/`.
