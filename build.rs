use std::process::Command;

fn main() {
    let git_commit = run_git(["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let git_dirty = run_git(["status", "--porcelain"])
        .map(|output| {
            if output.trim().is_empty() {
                "clean"
            } else {
                "dirty"
            }
            .to_string()
        })
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=RALPH_GIT_COMMIT={git_commit}");
    println!("cargo:rustc-env=RALPH_GIT_STATE={git_dirty}");
}

fn run_git<const N: usize>(args: [&str; N]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
