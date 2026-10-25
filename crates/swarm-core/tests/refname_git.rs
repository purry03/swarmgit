//! Cross-check `refname::is_valid_refname` against `git check-ref-format`.
//!
//! The M0 acceptance gate requires at least 5,000 generated names. If git is
//! not installed the test is skipped with a clear message rather than failing
//! in a non-development environment.

use std::process::Command;

use swarm_core::refname::{has_allowed_prefix, is_valid_refname};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0
    }

    fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        let index = (self.next() >> 33) as usize % values.len();
        &values[index]
    }
}

fn generated_name(rng: &mut Lcg) -> String {
    let prefixes = [
        "refs/heads/",
        "refs/tags/",
        "refs/heads/",
        "refs/tags/",
        "",
        "refs/meta/",
    ];
    let tokens = [
        "main",
        "feature",
        "x",
        "a.b",
        ".hidden",
        "x.lock",
        "x.",
        "x..y",
        "x@{y",
        "x y",
        "x~y",
        "x^y",
        "x:y",
        "x?y",
        "x*y",
        "x[y",
        "x\\y",
        "über",
        "",
        "-",
        "@",
        "release-1.0",
        "a_b",
        "HEAD",
    ];
    let prefix = *rng.pick(&prefixes);
    let count = 1 + (rng.next() % 4) as usize;
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        parts.push(*rng.pick(&tokens));
    }
    format!("{prefix}{}", parts.join("/"))
}

#[test]
fn refnames_agree_with_git_check_ref_format() {
    let git_available = Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !git_available {
        eprintln!("git not installed; skipping check-ref-format cross-check");
        return;
    }

    let mut rng = Lcg(0x5EED_1234_ABCD_0001);
    let mut compared = 0usize;
    for _ in 0..5_500 {
        let name = generated_name(&mut rng);
        let git_ok = Command::new("git")
            .args(["check-ref-format", &name])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
            && has_allowed_prefix(&name);
        assert_eq!(is_valid_refname(&name), git_ok, "disagreement on {name:?}");
        compared += 1;
    }
    assert!(compared >= 5_000);
}
