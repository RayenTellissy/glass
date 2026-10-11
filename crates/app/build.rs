//! Records the git commit and its date for the About dialog's version line
//! (`GLASS_GIT_COMMIT`, `GLASS_BUILD_DATE`). Builds from a source archive without git
//! leave both unset, and the dialog shows the version alone. The commit date rather
//! than the clock keeps builds reproducible.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (out.status.success() && !text.is_empty()).then(|| text.to_owned())
}

fn main() {
    // Rerun when the checkout moves to another commit: HEAD and its log live in this
    // worktree's git folder.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let dir = PathBuf::from(dir);
        println!("cargo:rerun-if-changed={}", dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", dir.join("logs/HEAD").display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(commit) = git(&["rev-parse", "--short=7", "HEAD"]) {
        println!("cargo:rustc-env=GLASS_GIT_COMMIT={commit}");
    }
    if let Some(date) = git(&["log", "-1", "--format=%cd", "--date=format:%Y.%m.%d"]) {
        println!("cargo:rustc-env=GLASS_BUILD_DATE={date}");
    }
}
