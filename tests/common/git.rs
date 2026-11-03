//! Git for disposable fixture repositories. Crate-independent: also mounted in-crate.
use std::{path::Path, process::Command};

/// Git confined to `dir`: explicit repository, work tree, index and config, so an inherited
/// hook environment (`GIT_DIR`, `GIT_INDEX_FILE`, ...) or a developer's config can never
/// redirect or alter a fixture commit.
pub fn git_command(dir: &Path) -> Command {
    let dir = std::path::absolute(dir).unwrap();
    let git_dir = dir.join(".git");
    let mut git = Command::new("git");
    git.current_dir(&dir)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &dir)
        .env("GIT_INDEX_FILE", git_dir.join("index"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ]);
    git
}

/// Runs `git args` in `dir` (see [`git_command`]), failing the test with Git's stderr.
pub fn git(dir: &Path, args: &[&str]) {
    let output = git_command(dir).args(args).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {args:?}: {stderr}");
}
