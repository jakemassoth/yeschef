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

/// Sanitize a branch name for use in derived window / zmx session names.
/// Replace any char not in `[a-z0-9-]` with `-`, collapse consecutive `-`,
/// strip leading/trailing `-`.
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

/// The brigade session name that namespaces every ticket window.
///
/// The real zmx backend maps each ticket window onto a standalone zmx session
/// named `<yeschef_session>-<window>`.
pub fn yeschef_session() -> &'static str {
    "yeschef"
}

/// Derive the ticket window name from project + sanitized branch.
///
/// Avoids `.` and `:` (historically tmux target separators) so the name stays
/// safe to embed in a zmx session id. `sanitize_branch` already strips `.`/`:`,
/// so a `-` join is safe.
pub fn window_name(project: &str, sanitized_branch: &str) -> String {
    format!("{project}-{sanitized_branch}")
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
    fn window_name_derivation() {
        assert_eq!(
            window_name("myproject", "feature-foo"),
            "myproject-feature-foo"
        );
    }

    #[test]
    fn window_name_is_target_safe() {
        // sanitize_branch + window_name must never produce `.` or `:`
        // (historically tmux target separators) so the name stays safe to
        // embed as a zmx session id.
        let w = window_name("proj", &sanitize_branch("feature/x.y:z"));
        assert!(!w.contains('.') && !w.contains(':'), "got {w}");
    }
}
