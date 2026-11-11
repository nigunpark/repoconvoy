#[path = "../src/workspace.rs"]
mod workspace;

use std::path::{Path, PathBuf};
use std::process::Command;
use workspace::Workspace;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "repoconvoy-workspace-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn repo(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir(&path).unwrap();
        git(&path, &["init", "-q"]);
        git(&path, &["config", "user.name", "Test Worker"]);
        git(&path, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(path.join("base.txt"), name).unwrap();
        git(&path, &["add", "base.txt"]);
        git(&path, &["commit", "-qm", "base"]);
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Git worktrees may have read-only files on Windows; fixture cleanup is best effort.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(args);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn prepare_preserves_source_and_ready_reads_committed_candidate() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    git(
        &source,
        &[
            "remote",
            "add",
            "origin",
            "https://127.0.0.1:1/unreachable.git",
        ],
    );
    let original = git(&source, &["rev-parse", "HEAD"]);
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    let candidate = workspace.prepare(&source).unwrap();
    assert_eq!(candidate.base_sha(), original);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), original);
    assert!(git(&source, &["status", "--porcelain"]).is_empty());
    assert!(git(candidate.path(), &["status", "--porcelain"]).is_empty());
    assert!(
        candidate.ready().is_err(),
        "unchanged candidate cannot be ready"
    );
    std::fs::write(candidate.path().join("change.txt"), "worker edit").unwrap();
    git(candidate.path(), &["add", "change.txt"]);
    git(candidate.path(), &["commit", "-qm", "candidate edit"]);
    let ready = candidate.ready().unwrap();
    assert_eq!(
        ready.head_sha,
        git(candidate.path(), &["rev-parse", "HEAD"])
    );
    assert!(ready.diff.contains("worker edit"));
    assert!(
        ready.diff.ends_with('\n'),
        "diff must preserve git patch bytes"
    );
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), original);
    assert!(git(&source, &["status", "--porcelain"]).is_empty());
    assert!(
        git(
            &source,
            &["for-each-ref", "--format=%(refname)", "refs/remotes"]
        )
        .is_empty()
    );
}

#[test]
fn two_sources_have_independent_single_writer_candidates() {
    let fixture = Fixture::new();
    let a = fixture.repo("a");
    let b = fixture.repo("b");
    let a_head = git(&a, &["rev-parse", "HEAD"]);
    let b_head = git(&b, &["rev-parse", "HEAD"]);
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    let a_path = workspace.prepare(&a).unwrap().path().to_owned();
    let b_path = workspace.prepare(&b).unwrap().path().to_owned();
    assert_ne!(a_path, b_path);
    assert_eq!(
        workspace.prepare(&a).err().unwrap().to_string(),
        "source already registered or overlaps registered source"
    );
    assert_eq!(
        workspace.prepare(a.join(".")).err().unwrap().to_string(),
        "source already registered or overlaps registered source"
    );
    std::fs::write(a_path.join("only-a.txt"), "A").unwrap();
    git(&a_path, &["add", "only-a.txt"]);
    git(&a_path, &["commit", "-qm", "edit A"]);
    std::fs::write(b_path.join("only-b.txt"), "B").unwrap();
    git(&b_path, &["add", "only-b.txt"]);
    git(&b_path, &["commit", "-qm", "edit B"]);
    let a_ready = workspace.candidate(&a).unwrap().ready().unwrap();
    let b_ready = workspace.candidate(&b).unwrap().ready().unwrap();
    assert!(a_ready.diff.contains("only-a.txt"));
    assert!(!a_ready.diff.contains("only-b.txt"));
    assert!(b_ready.diff.contains("only-b.txt"));
    assert!(!b_ready.diff.contains("only-a.txt"));
    assert_eq!(workspace.candidate(&a).unwrap().source(), a.as_path());
    assert_ne!(
        workspace.candidate(&a).unwrap().branch(),
        workspace.candidate(&b).unwrap().branch()
    );
    assert!(git(&a_path, &["show", "--format=", "--name-only", "HEAD"]).contains("only-a.txt"));
    assert!(git(&b_path, &["show", "--format=", "--name-only", "HEAD"]).contains("only-b.txt"));
    assert_eq!(git(&a, &["rev-parse", "HEAD"]), a_head);
    assert_eq!(git(&b, &["rev-parse", "HEAD"]), b_head);
    assert!(git(&a, &["status", "--porcelain"]).is_empty());
    assert!(git(&b, &["status", "--porcelain"]).is_empty());
    assert!(git(&a, &["remote", "-v"]).is_empty());
    assert!(git(&b, &["remote", "-v"]).is_empty());
}

#[test]
fn controller_rejects_second_writer_across_workspace_instances() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    let root = fixture.0.join("controller");
    let mut first = Workspace::new(&root).unwrap();
    let first_path = first.prepare(&source).unwrap().path().to_owned();
    let mut second = Workspace::new(&root).unwrap();
    assert!(
        second.prepare(&source).is_err(),
        "another controller instance cannot allocate a second writer"
    );
    assert!(first_path.exists());
}

#[test]
fn refuses_replacement_clone_at_candidate_path() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    let candidate = workspace.prepare(&source).unwrap();
    let path = candidate.path().to_owned();
    let displaced = fixture.0.join("displaced-worktree");
    std::fs::rename(&path, &displaced).unwrap();

    git(
        &fixture.0,
        &[
            "clone",
            "-q",
            source.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    git(&path, &["config", "user.name", "Test Worker"]);
    git(&path, &["config", "user.email", "test@example.invalid"]);
    git(&path, &["checkout", "-qb", candidate.branch()]);
    std::fs::write(path.join("change.txt"), "substituted checkout").unwrap();
    git(&path, &["add", "change.txt"]);
    git(&path, &["commit", "-qm", "replacement edit"]);
    assert!(
        candidate.ready().is_err(),
        "a clone at the same path with a valid descendant is not the registered worktree"
    );
}

#[test]
fn refuses_nested_source_and_overlapping_output_without_mutation() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    std::fs::create_dir(source.join("nested")).unwrap();
    let head = git(&source, &["rev-parse", "HEAD"]);
    let mut outside = Workspace::new(fixture.0.join("controller")).unwrap();
    assert!(outside.prepare(source.join("nested")).is_err());
    let mut inside = Workspace::new(source.join("output")).unwrap();
    assert!(inside.prepare(&source).is_err());
    assert!(
        Workspace::new(source.join("..").join("source").join("output")).is_err(),
        "output containing parent traversal must be rejected"
    );
    assert!(!source.join("output").exists());
    let mut containing = Workspace::new(&fixture.0).unwrap();
    assert!(containing.prepare(&source).is_err());
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), head);
    assert!(git(&source, &["status", "--porcelain"]).is_empty());
}

#[test]
fn refuses_dirty_source_and_uncommitted_candidate() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    std::fs::write(source.join("untracked.txt"), "dirty").unwrap();
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    assert!(workspace.prepare(&source).is_err());
    assert!(!fixture.0.join("controller").exists());
    std::fs::remove_file(source.join("untracked.txt")).unwrap();
    let candidate = workspace.prepare(&source).unwrap();
    std::fs::write(candidate.path().join("untracked.txt"), "dirty").unwrap();
    assert!(candidate.ready().is_err());
}

#[test]
fn refuses_source_dirtied_after_candidate_is_prepared() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    let candidate = workspace.prepare(&source).unwrap();
    std::fs::write(candidate.path().join("change.txt"), "committed change").unwrap();
    git(candidate.path(), &["add", "change.txt"]);
    git(candidate.path(), &["commit", "-qm", "candidate change"]);
    assert!(
        candidate.ready().is_ok(),
        "clean committed candidate is ready"
    );

    std::fs::write(source.join("untracked.txt"), "source dirtied later").unwrap();
    assert_eq!(
        candidate.ready().err().map(|error| error.to_string()),
        Some("source has uncommitted or untracked changes".to_owned()),
        "an unchanged source HEAD is not enough when the source worktree is dirty"
    );
}

#[test]
fn refuses_commits_with_no_net_diff_and_source_head_drift() {
    let fixture = Fixture::new();
    let source = fixture.repo("source");
    let mut workspace = Workspace::new(fixture.0.join("controller")).unwrap();
    let candidate = workspace.prepare(&source).unwrap();
    std::fs::write(candidate.path().join("base.txt"), "changed").unwrap();
    git(candidate.path(), &["add", "base.txt"]);
    git(candidate.path(), &["commit", "-qm", "change"]);
    git(candidate.path(), &["revert", "--no-edit", "HEAD"]);
    assert!(
        candidate.ready().is_err(),
        "commits reverted to base are not a result"
    );
    std::fs::write(source.join("new.txt"), "source change").unwrap();
    git(&source, &["add", "new.txt"]);
    git(&source, &["commit", "-qm", "source moved"]);
    assert!(
        candidate.ready().is_err(),
        "original source HEAD must remain pinned"
    );
}
