use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_FILENAME: &str = "tellur-release-notes.json";
const DEFAULT_CHANGESET_DIR: &str = ".changeset";
const DEFAULT_CHANGELOG: &str = "CHANGELOG.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Bump {
    Major,
    Minor,
    Patch,
}

impl Bump {
    fn heading(self) -> &'static str {
        match self {
            Bump::Major => "Breaking Changes",
            Bump::Minor => "Features",
            Bump::Patch => "Fixes",
        }
    }
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    default: Bump,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Change {
    path: String,
    bump: Bump,
    summary: String,
    details: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    changes: Vec<Change>,
}

#[derive(Parser, Debug)]
#[command(name = "release-notes", about = "Render changelog entries from changesets")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Validate changesets and write a JSON manifest for later apply.
    Collect {
        #[arg(long, default_value = DEFAULT_CHANGESET_DIR)]
        changeset_dir: PathBuf,
        /// Defaults to `<git-dir>/tellur-release-notes.json` (works with worktrees).
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Rewrite the newest CHANGELOG entry using a previously collected manifest.
    Apply {
        #[arg(long, default_value = DEFAULT_CHANGELOG)]
        changelog: PathBuf,
        /// Defaults to `<git-dir>/tellur-release-notes.json` (works with worktrees).
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Validate changesets without writing output.
    Check {
        #[arg(long, default_value = DEFAULT_CHANGESET_DIR)]
        changeset_dir: PathBuf,
    },
    /// Print the changelog entry that would be added, without writing files.
    Preview {
        #[arg(long, default_value = DEFAULT_CHANGESET_DIR)]
        changeset_dir: PathBuf,
        #[arg(long, default_value = "Cargo.toml")]
        cargo_toml: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Collect {
            changeset_dir,
            manifest,
        } => {
            let manifest = resolve_manifest_path(manifest)?;
            collect(&changeset_dir, &manifest)
        }
        Commands::Apply {
            changelog,
            manifest,
        } => {
            let manifest = resolve_manifest_path(manifest)?;
            apply(&changelog, &manifest)
        }
        Commands::Check { changeset_dir } => {
            let changes = load_changesets(&changeset_dir)?;
            println!("Validated {} changeset(s).", changes.len());
            for change in changes {
                println!(
                    "  {}  bump={}  {}",
                    change.path,
                    bump_name(change.bump),
                    change.summary
                );
            }
            Ok(())
        }
        Commands::Preview {
            changeset_dir,
            cargo_toml,
        } => preview(&changeset_dir, &cargo_toml),
    }
}

fn resolve_manifest_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    Ok(git_dir()?.join(MANIFEST_FILENAME))
}

fn git_dir() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .context("failed to run `git rev-parse --git-dir`")?;
    if !output.status.success() {
        bail!(
            "`git rev-parse --git-dir` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let path = String::from_utf8(output.stdout)
        .context("git-dir path was not valid UTF-8")?
        .trim()
        .to_string();
    if path.is_empty() {
        bail!("`git rev-parse --git-dir` returned an empty path");
    }
    Ok(PathBuf::from(path))
}

fn bump_name(bump: Bump) -> &'static str {
    match bump {
        Bump::Major => "major",
        Bump::Minor => "minor",
        Bump::Patch => "patch",
    }
}

fn collect(changeset_dir: &Path, manifest_path: &Path) -> Result<()> {
    let changes = load_changesets(changeset_dir)?;
    if let Some(parent) = manifest_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let manifest = Manifest { changes };
    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(manifest_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    println!(
        "Wrote {} change(s) to {}",
        manifest.changes.len(),
        manifest_path.display()
    );
    Ok(())
}

fn apply(changelog_path: &Path, manifest_path: &Path) -> Result<()> {
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let changelog = fs::read_to_string(changelog_path)
        .with_context(|| format!("failed to read {}", changelog_path.display()))?;
    let rewritten = rewrite_newest_entry(&changelog, &manifest.changes)?;
    fs::write(changelog_path, rewritten)
        .with_context(|| format!("failed to write {}", changelog_path.display()))?;
    println!("Rewrote newest entry in {}", changelog_path.display());
    let _ = fs::remove_file(manifest_path);
    Ok(())
}

fn preview(changeset_dir: &Path, cargo_toml: &Path) -> Result<()> {
    let changes = load_changesets(changeset_dir)?;
    let current = read_workspace_version(cargo_toml)?;
    let next = next_version(&current, highest_bump(&changes))?;
    let today = chrono_like_today();
    let entry = format!(
        "## {next} ({today})\n{sections}",
        sections = render_sections(&changes)
    );

    let current_s = format!("{}.{}.{}", current.0, current.1, current.2);
    println!("Would bump version: {current_s} -> {next}");
    println!("Would consume {} changeset(s):", changes.len());
    for change in &changes {
        println!(
            "  {}  bump={}",
            change.path,
            bump_name(change.bump)
        );
    }
    println!();
    println!("Would add the following to CHANGELOG.md:");
    println!();
    print!("{entry}");
    Ok(())
}

fn highest_bump(changes: &[Change]) -> Bump {
    changes
        .iter()
        .map(|change| change.bump)
        .max_by_key(|bump| match bump {
            Bump::Patch => 0,
            Bump::Minor => 1,
            Bump::Major => 2,
        })
        .unwrap_or(Bump::Patch)
}

fn read_workspace_version(cargo_toml: &Path) -> Result<(u64, u64, u64)> {
    let text = fs::read_to_string(cargo_toml)
        .with_context(|| format!("failed to read {}", cargo_toml.display()))?;
    let mut in_workspace_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if !in_workspace_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim();
            let Some(rest) = rest.strip_prefix('"') else {
                continue;
            };
            let Some(end) = rest.find('"') else {
                continue;
            };
            let version = &rest[..end];
            let parts = version.split('.').collect::<Vec<_>>();
            if parts.len() != 3 {
                bail!("unexpected version format in {}: {version}", cargo_toml.display());
            }
            return Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?));
        }
    }
    bail!(
        "no [workspace.package] version found in {}",
        cargo_toml.display()
    );
}

fn next_version(current: &(u64, u64, u64), bump: Bump) -> Result<String> {
    let (major, minor, patch) = *current;
    // Match knope 0.22.x 0.y.z rules:
    //   major -> bump minor (0.1.0 -> 0.2.0)
    //   minor -> bump patch (0.1.0 -> 0.1.1)
    //   patch -> bump patch
    let (major, minor, patch) = match bump {
        Bump::Major => {
            if major == 0 {
                (0, minor + 1, 0)
            } else {
                (major + 1, 0, 0)
            }
        }
        Bump::Minor => {
            if major == 0 {
                (0, minor, patch + 1)
            } else {
                (major, minor + 1, 0)
            }
        }
        Bump::Patch => (major, minor, patch + 1),
    };
    Ok(format!("{major}.{minor}.{patch}"))
}

fn chrono_like_today() -> String {
    // Prefer local date via `date`; fall back to UTC from the process if needed.
    if let Ok(output) = std::process::Command::new("date").args(["+%Y-%m-%d"]).output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "YYYY-MM-DD".to_string()
}

fn load_changesets(dir: &Path) -> Result<Vec<Change>> {
    if !dir.is_dir() {
        bail!("changeset directory not found: {}", dir.display());
    }

    let mut paths = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect::<Vec<_>>();
    paths.sort();

    if paths.is_empty() {
        bail!("no .changeset/*.md files found in {}", dir.display());
    }

    let mut changes = Vec::with_capacity(paths.len());
    for path in paths {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let change = parse_changeset(&text)
            .with_context(|| format!("invalid changeset {}", path.display()))?;
        changes.push(Change {
            path: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown.md")
                .to_string(),
            bump: change.0,
            summary: change.1,
            details: change.2,
        });
    }
    Ok(changes)
}

fn parse_changeset(text: &str) -> Result<(Bump, String, Option<String>)> {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let Some(rest) = text.strip_prefix("---") else {
        bail!("missing YAML front matter (expected leading ---)");
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    let Some(end) = rest.find("\n---") else {
        bail!("unterminated YAML front matter (expected closing ---)");
    };
    let yaml = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);

    let front: FrontMatter = serde_yaml::from_str(yaml).context("invalid YAML front matter")?;

    let body = body.trim();
    if body.is_empty() {
        bail!("changeset body is empty; expected an H1 summary");
    }

    let mut lines = body.lines();
    let heading = lines
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .context("changeset body is empty; expected an H1 summary")?;
    let Some(summary) = heading.strip_prefix("# ") else {
        bail!("changeset summary must be an H1 (`# ...`), got: {heading}");
    };
    let summary = summary.trim();
    if summary.is_empty() {
        bail!("changeset H1 summary is empty");
    }

    let details = lines.collect::<Vec<_>>().join("\n");
    let details = {
        let trimmed = details.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    Ok((front.default, summary.to_string(), details))
}

fn rewrite_newest_entry(changelog: &str, changes: &[Change]) -> Result<String> {
    let version_re = Regex::new(r"(?m)^##\s+.+$").unwrap();
    let mut matches = version_re.find_iter(changelog);
    let first = matches
        .next()
        .context("CHANGELOG.md has no version heading (`## ...`)")?;
    let second_start = matches.next().map(|m| m.start());

    let prefix = &changelog[..first.start()];
    let heading_line = first.as_str();
    let old_body_end = second_start.unwrap_or(changelog.len());
    let suffix = &changelog[old_body_end..];

    let mut rendered = String::new();
    rendered.push_str(prefix);
    rendered.push_str(heading_line);
    rendered.push('\n');
    rendered.push_str(&render_sections(changes));
    if !suffix.is_empty() {
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        // Keep a blank line between version entries when a previous entry follows.
        if !suffix.starts_with('\n') {
            rendered.push('\n');
        }
        rendered.push_str(suffix);
    } else if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn render_sections(changes: &[Change]) -> String {
    let mut by_bump: Vec<(Bump, Vec<&Change>)> = Vec::new();
    for bump in [Bump::Major, Bump::Minor, Bump::Patch] {
        let items = changes
            .iter()
            .filter(|change| change.bump == bump)
            .collect::<Vec<_>>();
        if !items.is_empty() {
            by_bump.push((bump, items));
        }
    }

    let mut out = String::new();
    for (bump, items) in by_bump {
        out.push('\n');
        out.push_str("### ");
        out.push_str(bump.heading());
        out.push('\n');

        for change in items {
            out.push('\n');
            out.push_str("- ");
            if change.details.is_some() {
                out.push_str("**");
                out.push_str(&change.summary);
                out.push_str("**");
            } else {
                out.push_str(&change.summary);
            }
            out.push('\n');

            if let Some(details) = &change.details {
                out.push('\n');
                out.push_str(&indent_detail_lines(details));
                out.push('\n');
            }
        }
    }
    out.push('\n');
    out
}

fn indent_detail_lines(details: &str) -> String {
    details
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parses_simple_changeset() {
        let text = "\
---
default: minor
---

# Fix renderer cache invalidation
";
        let (bump, summary, details) = parse_changeset(text).unwrap();
        assert_eq!(bump, Bump::Minor);
        assert_eq!(summary, "Fix renderer cache invalidation");
        assert!(details.is_none());
    }

    #[test]
    fn parses_complex_changeset() {
        let text = "\
---
default: patch
---

# Add an AI agent authoring skill

Longer description with details.
";
        let (bump, summary, details) = parse_changeset(text).unwrap();
        assert_eq!(bump, Bump::Patch);
        assert_eq!(summary, "Add an AI agent authoring skill");
        assert_eq!(details.as_deref(), Some("Longer description with details."));
    }

    #[test]
    fn accepts_changeset_without_section() {
        let text = "\
---
default: patch
---

# Something
";
        let (bump, summary, details) = parse_changeset(text).unwrap();
        assert_eq!(bump, Bump::Patch);
        assert_eq!(summary, "Something");
        assert!(details.is_none());
    }

    #[test]
    fn rewrites_newest_entry_preserving_older() {
        let changelog = "\
# Changelog

## 0.2.0 (2026-07-09)

### Features

- wrong place

## 0.1.0 (2026-07-07)

### Features

- Initial public release
";
        let changes = vec![
            Change {
                path: "a.md".into(),
                bump: Bump::Minor,
                summary: "Add skill".into(),
                details: None,
            },
            Change {
                path: "b.md".into(),
                bump: Bump::Patch,
                summary: "Fix cache invalidation".into(),
                details: Some("More detail.".into()),
            },
        ];
        let rewritten = rewrite_newest_entry(changelog, &changes).unwrap();
        assert!(rewritten.contains("## 0.2.0 (2026-07-09)"));
        assert!(rewritten.contains("### Features"));
        assert!(rewritten.contains("- Add skill"));
        assert!(rewritten.contains("### Fixes"));
        assert!(rewritten.contains("- **Fix cache invalidation**"));
        assert!(rewritten.contains("  More detail."));
        assert!(!rewritten.contains("#### Fix cache invalidation"));
        assert!(!rewritten.contains("- wrong place"));
        assert!(rewritten.contains("## 0.1.0 (2026-07-07)"));
        assert!(rewritten.contains("- Initial public release"));
    }

    #[test]
    fn renders_complex_changes_as_bullet_with_indented_details() {
        let changes = vec![Change {
            path: "a.md".into(),
            bump: Bump::Major,
            summary: "The write-on effect now draws at a constant speed per path by default".into(),
            details: Some(
                "Previously, the timing was controlled to write the stroke at a constant overall rate.\n\nTo resolve this, we have introduced .per_path().".into(),
            ),
        }];
        let rendered = render_sections(&changes);
        assert!(rendered.contains("### Breaking Changes"));
        assert!(rendered.contains(
            "- **The write-on effect now draws at a constant speed per path by default**"
        ));
        assert!(rendered.contains(
            "  Previously, the timing was controlled to write the stroke at a constant overall rate."
        ));
        assert!(rendered.contains("  To resolve this, we have introduced .per_path()."));
        assert!(!rendered.contains("#### "));
    }

    #[test]
    fn collect_and_apply_roundtrip() {
        let dir = tempdir().unwrap();
        let changeset_dir = dir.path().join(".changeset");
        fs::create_dir_all(&changeset_dir).unwrap();
        let mut file = fs::File::create(changeset_dir.join("demo.md")).unwrap();
        write!(
            file,
            "---\ndefault: patch\n---\n\n# Fix something\n"
        )
        .unwrap();

        let changelog_path = dir.path().join("CHANGELOG.md");
        fs::write(
            &changelog_path,
            "# Changelog\n\n## 0.2.0 (2026-07-09)\n\n### Features\n\n- placeholder\n",
        )
        .unwrap();

        let manifest = dir.path().join("manifest.json");
        collect(&changeset_dir, &manifest).unwrap();
        apply(&changelog_path, &manifest).unwrap();

        let out = fs::read_to_string(&changelog_path).unwrap();
        assert!(out.contains("### Fixes"));
        assert!(out.contains("- Fix something"));
        assert!(!manifest.exists());
    }

    #[test]
    fn render_sections_groups_by_bump() {
        let changes = vec![
            Change {
                path: "a.md".into(),
                bump: Bump::Minor,
                summary: "Add skill".into(),
                details: None,
            },
            Change {
                path: "b.md".into(),
                bump: Bump::Patch,
                summary: "Fix cache invalidation".into(),
                details: None,
            },
        ];
        let rendered = render_sections(&changes);
        assert!(rendered.contains("### Features"));
        assert!(rendered.contains("- Add skill"));
        assert!(rendered.contains("### Fixes"));
        assert!(rendered.contains("- Fix cache invalidation"));
        assert_eq!(highest_bump(&changes), Bump::Minor);
        assert_eq!(next_version(&(0, 1, 0), Bump::Major).unwrap(), "0.2.0");
        assert_eq!(next_version(&(0, 1, 0), Bump::Minor).unwrap(), "0.1.1");
        assert_eq!(next_version(&(0, 1, 0), Bump::Patch).unwrap(), "0.1.1");
        assert_eq!(next_version(&(1, 2, 3), Bump::Major).unwrap(), "2.0.0");
        assert_eq!(next_version(&(1, 2, 3), Bump::Minor).unwrap(), "1.3.0");
        assert_eq!(next_version(&(1, 2, 3), Bump::Patch).unwrap(), "1.2.4");
    }
}
