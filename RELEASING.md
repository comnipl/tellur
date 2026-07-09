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
- Allowed bump kinds: `major`, `minor`, `patch`.
- Do not put other Markdown files in `.changeset/` — knope treats every `*.md` there as a change file.

## Cut a release

1. Merge feature PRs (with changesets) into `main`.
2. Run **Prepare Release** via `workflow_dispatch` (Actions → Prepare Release → Run workflow).
3. Review the opened `chore: release vX.Y.Z` PR (version bumps, `CHANGELOG.md`, publish dry-run CI).
4. Merge the Release PR. That tags `vX.Y.Z`, creates the GitHub Release, and publishes all crates to crates.io via Trusted Publishing.
