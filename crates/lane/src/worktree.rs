//! Worktree provisioning owned by Runtime (not Fleet) — #4176 / #4016.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::DateTime;

/// Spec for an isolated worktree + branch for a lane.
#[derive(Debug, Clone)]
pub struct WorktreeProvision {
    /// Git repository root (must contain `.git`).
    pub repo_root: PathBuf,
    /// Branch to create (from `base_ref`).
    pub branch: String,
    /// Directory for the new worktree (created by `git worktree add`).
    pub path: PathBuf,
    /// Base ref to branch from (default `HEAD`).
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProvisionedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Create a git worktree + branch for a lane.
pub fn provision_worktree(spec: &WorktreeProvision) -> Result<ProvisionedWorktree> {
    if spec.branch.trim().is_empty() {
        bail!("worktree branch must not be empty");
    }
    if !spec.repo_root.exists() {
        bail!("repo root does not exist: {}", spec.repo_root.display());
    }
    if let Some(parent) = spec.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }
    let base = spec.base_ref.as_deref().unwrap_or("HEAD");
    // Capture git output instead of inheriting the caller's terminal. Runtime
    // callers include the raw-mode TUI launch screen, where even one inherited
    // progress/error line corrupts the alternate-screen buffer.
    let output = Command::new("git")
        .current_dir(&spec.repo_root)
        .args([
            "worktree",
            "add",
            "-b",
            &spec.branch,
            &spec.path.to_string_lossy(),
            base,
        ])
        .output()
        .context("git worktree add")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git worktree add failed for branch {} at {}{}{}",
            spec.branch,
            spec.path.display(),
            if detail.is_empty() { "" } else { ": " },
            detail
        );
    }
    Ok(ProvisionedWorktree {
        path: spec.path.clone(),
        branch: spec.branch.clone(),
    })
}

/// Remove a worktree when TTL has expired (or immediately when TTL is 0).
///
/// `stopped_at` is RFC3339. When `ttl_secs` is `None`, no cleanup is performed.
pub fn remove_worktree_if_expired(
    worktree_path: &Path,
    ttl_secs: Option<u64>,
    stopped_at: Option<&str>,
) -> Result<()> {
    let Some(ttl) = ttl_secs else {
        return Ok(());
    };
    if !worktree_path.exists() {
        return Ok(());
    }
    if ttl > 0 {
        let Some(stopped) = stopped_at else {
            return Ok(());
        };
        let stopped_ts = DateTime::parse_from_rfc3339(stopped)
            .with_context(|| format!("parse stopped_at {stopped}"))?
            .timestamp() as u64;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(stopped_ts) < ttl {
            return Ok(());
        }
    }

    // Resolve the branch this worktree has checked out *before* deleting the
    // directory — afterwards the information is gone. #4731: cleaning up the
    // directory but leaving the branch behind accumulates stale branches and
    // makes re-provisioning a lane under the same launch name fail with
    // "branch already exists".
    let branch = git_capture(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|branch| branch != "HEAD");
    let git_common_dir = git_capture(
        worktree_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );

    // Best-effort: git worktree remove --force, then rm -rf. Run it from the
    // owning repository when we know it — otherwise git resolves the process
    // cwd's repo, which has nothing to do with this worktree.
    let mut remove = Command::new("git");
    if let Some(root) = git_common_dir
        .as_deref()
        .and_then(|d| Path::new(d).parent())
    {
        remove.current_dir(root);
    }
    let _ = remove
        .args([
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ])
        .status();
    if worktree_path.exists() {
        fs::remove_dir_all(worktree_path)
            .with_context(|| format!("remove worktree {}", worktree_path.display()))?;
    }

    if let (Some(branch), Some(git_dir)) = (branch, git_common_dir) {
        delete_lane_branch(&git_dir, &branch);
    }
    Ok(())
}

/// Run a git command in `dir` and return trimmed stdout, or `None` when git
/// fails / prints nothing. Purely informational: never fails the caller.
fn git_capture(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// Delete the branch a removed lane worktree was on (#4731).
///
/// Deliberately a *safe* delete (`git branch -d`): a lane branch whose commits
/// are not reachable from anywhere else is real work, and TTL expiry is not
/// consent to destroy it. Branches with unmerged commits are kept and logged;
/// everything else (the common case — a lane that produced no commits, or whose
/// work was already merged) is removed so the name is free for reuse.
fn delete_lane_branch(git_dir: &str, branch: &str) {
    // The worktree directory may have been removed with `rm -rf`, in which case
    // git still holds an administrative entry that pins the branch.
    let _ = Command::new("git")
        .args(["--git-dir", git_dir, "worktree", "prune"])
        .output();
    let output = Command::new("git")
        .args(["--git-dir", git_dir, "branch", "-d", branch])
        .output();
    match output {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            tracing::warn!(
                branch,
                git_dir,
                detail = %String::from_utf8_lossy(&output.stderr).trim(),
                "lane worktree removed but its branch was kept (unmerged commits or delete refused)"
            );
        }
        Err(err) => {
            tracing::warn!(branch, git_dir, %err, "failed to run git branch -d for expired lane worktree");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo(root: &Path) {
        assert!(
            Command::new("git")
                .args(["init", "-b", "main"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "lane@test"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "lane"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("README"), "lane").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "README"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-m", "init"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn provision_and_ttl_zero_cleanup() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-lane");
        let provisioned = provision_worktree(&WorktreeProvision {
            repo_root: repo,
            branch: "codex/lane-test".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();
        assert!(provisioned.path.is_dir());
        assert!(wt_path.join("README").is_file());

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            !wt_path.exists(),
            "TTL 0 should remove worktree immediately"
        );
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        Command::new("git")
            .current_dir(repo)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// #4731: expired cleanup used to delete the directory and leak the branch,
    /// so re-provisioning the same launch name failed with "branch already exists".
    #[test]
    fn expired_cleanup_deletes_branch_and_allows_reprovision() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-reuse");
        let spec = WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/reused-name".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        };
        provision_worktree(&spec).unwrap();
        assert!(branch_exists(&repo, "codex/reused-name"));

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(!wt_path.exists());
        assert!(
            !branch_exists(&repo, "codex/reused-name"),
            "expired worktree cleanup must delete its branch, not just the directory"
        );

        // The whole point: the same launch name is usable again.
        provision_worktree(&spec).expect("re-provisioning the same launch name must succeed");
        assert!(wt_path.join("README").is_file());
    }

    /// A lane that produced commits is not garbage: TTL expiry removes the
    /// directory but must not destroy unmerged work.
    #[test]
    fn expired_cleanup_keeps_branch_with_unmerged_commits() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-work");
        provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/has-work".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();
        fs::write(wt_path.join("work.txt"), "lane work").unwrap();
        for args in [vec!["add", "work.txt"], vec!["commit", "-m", "lane work"]] {
            assert!(
                Command::new("git")
                    .args(&args)
                    .current_dir(&wt_path)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(!wt_path.exists());
        assert!(
            branch_exists(&repo, "codex/has-work"),
            "unmerged lane commits must survive TTL cleanup"
        );
    }
}
