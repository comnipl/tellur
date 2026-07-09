# Releasing tellur

Lockstep releases for all workspace crates, driven by [Knope](https://knope.tech) changesets and GitHub Actions.

## Document a change

Any PR that should appear in the next release notes must include a changeset under `.changeset/`.

Generate one interactively:

```bash
knope document-change
```

Then edit the generated file to add a `section` field (Knope's interactive prompt only covers the SemVer bump).

Or hand-write a file such as `.changeset/my-change.md`:

```markdown
---
default: patch
section: features
---

# Short summary

Optional longer Markdown description for the changelog.
```

- The package name in the front matter is always `default` (knope's name for the single anonymous `[package]` that covers the whole workspace in lockstep).
- `default` is the SemVer bump: `major`, `minor`, or `patch`.
- `section` is the changelog category and is independent of the bump: `breaking`, `features`, or `fixes`.
- Examples of intentional mismatches:
  - `default: minor` + `section: fixes` — a fix that warrants a minor bump
  - `default: patch` + `section: features` — a small additive change that stays patch
  - `default: major` + `section: breaking` — a breaking change
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

This prints the SemVer bump implied by pending changesets and the changelog sections that would be written (`section`, not `default`). It does not touch `CHANGELOG.md`, `Cargo.toml`, or `.changeset/`.
