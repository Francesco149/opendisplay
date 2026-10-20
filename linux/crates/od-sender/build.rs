//! `OD_SENDER_VERSION` for `od-sender --version`, first of:
//!
//! 1. `$OD_SENDER_VERSION`, set by the release workflow and the PKGBUILD
//!    (a tag tarball has no `.git`);
//! 2. `git describe` against the `linux-v*` tags: `0.1.0-beta.1` on the
//!    tag, `0.1.0-beta.1-5-gabc1234` past it, `0.1.0-dev.abc1234` before
//!    the first one;
//! 3. the crate version.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn main() {
    println!("cargo:rerun-if-env-changed=OD_SENDER_VERSION");
    let crate_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let from_env = std::env::var("OD_SENDER_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let version = from_env
        .or_else(|| {
            // A new commit or checkout changes HEAD (or the branch's log).
            if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
                println!("cargo:rerun-if-changed={dir}/HEAD");
                println!("cargo:rerun-if-changed={dir}/logs/HEAD");
            }
            let d = git(&["describe", "--tags", "--match", "linux-v*", "--always"])?;
            Some(match d.strip_prefix("linux-v") {
                Some(tagged) => tagged.to_string(),
                None => format!("{crate_version}-dev.{d}"),
            })
        })
        .unwrap_or(crate_version);
    println!("cargo:rustc-env=OD_SENDER_VERSION={version}");
}
