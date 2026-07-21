use anyhow::{bail, Result};

/// Validate a project name.
/// Valid: matches `^[a-z0-9][a-z0-9-]*[a-z0-9]$` OR single char `^[a-z0-9]$`.
/// Consecutive hyphens are rejected.
pub fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("project name must not be empty");
    }
    // Single char
    if name.len() == 1 {
        let c = name.chars().next().unwrap();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            return Ok(());
        }
        bail!("project name '{name}' is invalid: must be lowercase alphanumeric or hyphen");
    }
    // Multi-char: no consecutive hyphens, no leading/trailing hyphen, lowercase only
    if name.starts_with('-') {
        bail!("project name '{name}' must not start with a hyphen");
    }
    if name.ends_with('-') {
        bail!("project name '{name}' must not end with a hyphen");
    }
    if name.contains("--") {
        bail!("project name '{name}' must not contain consecutive hyphens");
    }
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            bail!(
                "project name '{name}' contains invalid character '{c}': only lowercase letters, digits, and hyphens are allowed"
            );
        }
    }
    Ok(())
}

/// Derive a project name from a git URL (basename with .git stripped).
pub fn name_from_url(url: &str) -> String {
    // Strip trailing slash
    let url = url.trim_end_matches('/');
    // Take the last path component
    let base = url.rsplit('/').next().unwrap_or(url);
    // Strip .git suffix
    let base = base.strip_suffix(".git").unwrap_or(base);
    // Lowercase and replace invalid chars with hyphens
    sanitize_for_project(base)
}

fn sanitize_for_project(s: &str) -> String {
    let lower = s.to_lowercase();
    // Replace any non-alphanumeric, non-hyphen char with hyphen
    let mut result: String = lower
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse consecutive hyphens
    while result.contains("--") {
        result = result.replace("--", "-");
    }
    // Strip leading/trailing hyphens
    result = result.trim_matches('-').to_string();
    result
}

/// Sanitize a branch name into a filesystem-safe token, used for the per-ticket
/// spawn prompt file name (`<project>-<sanitized>.md`). Replace any char not in
/// `[a-z0-9-]` with `-`, collapse consecutive `-`, strip leading/trailing `-`.
pub fn sanitize_branch(branch: &str) -> String {
    let lower = branch.to_lowercase();
    let mut result: String = lower
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse consecutive hyphens
    while result.contains("--") {
        result = result.replace("--", "-");
    }
    // Strip leading/trailing hyphens
    result = result.trim_matches('-').to_string();
    result
}

/// The label of the pinned head-chef workspace in the brigade herdr session — a
/// Claude Code session running in the yeschef source checkout. yeschef looks up
/// the head chef by this label (see `commands::orchestrate::ensure_brigade`);
/// cook workspaces are labelled `<project>/<branch>` (see [`workspace_label`]),
/// which always contains a `/`, so a cook can never collide with this label.
pub fn headchef_label() -> &'static str {
    "headchef"
}

/// The label of a line cook's herdr workspace: `<project>/<branch>`. Purely
/// human-facing (shown in herdr's TUI) — yeschef matches a ticket to its
/// workspace by the stored `workspace_id`, not by this label, so labels need not
/// be unique. herdr accepts `/` in labels.
pub fn workspace_label(project: &str, branch: &str) -> String {
    format!("{project}/{branch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_project_names() {
        assert!(validate_project_name("foo").is_ok());
        assert!(validate_project_name("my-project").is_ok());
        assert!(validate_project_name("a1b2").is_ok());
        assert!(validate_project_name("a").is_ok());
        assert!(validate_project_name("1").is_ok());
    }

    #[test]
    fn invalid_project_names() {
        assert!(validate_project_name("").is_err(), "empty");
        assert!(validate_project_name("foo/bar").is_err(), "slash");
        assert!(validate_project_name("foo bar").is_err(), "space");
        assert!(validate_project_name("-foo").is_err(), "leading hyphen");
        assert!(validate_project_name("foo-").is_err(), "trailing hyphen");
        assert!(validate_project_name("FOO").is_err(), "uppercase");
        assert!(
            validate_project_name("foo--bar").is_err(),
            "consecutive hyphens"
        );
    }

    #[test]
    fn branch_sanitization() {
        assert_eq!(sanitize_branch("feature/foo"), "feature-foo");
        assert_eq!(sanitize_branch("my branch"), "my-branch");
        assert_eq!(sanitize_branch("main"), "main");
        assert_eq!(sanitize_branch("feature/foo/bar"), "feature-foo-bar");
        assert_eq!(sanitize_branch("UPPER"), "upper");
    }

    #[test]
    fn workspace_label_derivation() {
        // The cook workspace label is the human-facing `<project>/<branch>`,
        // keeping the real branch name (slashes and all) for herdr's TUI.
        assert_eq!(
            workspace_label("myproject", "feature/foo"),
            "myproject/feature/foo"
        );
        assert_eq!(workspace_label("proj", "main"), "proj/main");
    }

    #[test]
    fn headchef_label_never_collides_with_a_cook() {
        // A cook label always contains a `/` (project/branch); the head-chef
        // label never does, so they can't collide.
        assert!(!headchef_label().contains('/'));
        assert!(workspace_label("proj", "x").contains('/'));
    }
}
