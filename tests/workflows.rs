//! L-8: the GitHub workflows. Every action is pinned to a full commit SHA
//! with its version in a trailing comment, the default token is read-only,
//! and only the release job may sign a build provenance attestation.

use std::path::Path;

fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    out.sort();
    assert_eq!(out.len(), 3, "ci, nightly, release");
    out
}

#[test]
fn every_action_is_pinned_to_a_full_commit_sha_with_its_version() {
    let mut seen = 0;
    for (name, text) in workflows() {
        for line in text.lines().filter(|l| {
            l.trim_start().starts_with("- uses:") || l.trim_start().starts_with("uses:")
        }) {
            seen += 1;
            let rest = line.split_once("uses:").unwrap().1.trim();
            let (action, comment) = rest
                .split_once(" # ")
                .unwrap_or_else(|| panic!("{name}: no version comment: {line}"));
            let (repo, sha) = action
                .split_once('@')
                .unwrap_or_else(|| panic!("{name}: no ref: {line}"));
            assert!(repo.contains('/'), "{name}: {line}");
            assert!(
                sha.len() == 40 && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{name}: not a full commit SHA: {line}"
            );
            assert!(
                comment.starts_with('v') || comment.starts_with("stable"),
                "{name}: the comment must name the version: {line}"
            );
        }
    }
    assert!(seen >= 10, "found only {seen} `uses:` lines");
}

#[test]
fn the_toolchain_action_names_its_toolchain_now_that_the_ref_is_a_sha() {
    // dtolnay/rust-toolchain reads the toolchain from its ref (`@stable`);
    // pinned to a SHA it needs the input.
    for (name, text) in workflows() {
        let uses = text.matches("dtolnay/rust-toolchain@").count();
        let inputs = text.matches("toolchain: stable").count();
        assert_eq!(uses, inputs, "{name}");
    }
}

#[test]
fn the_default_token_is_read_only_everywhere() {
    for (name, text) in workflows() {
        assert!(
            text.contains("\npermissions:\n  contents: read\n"),
            "{name}: top-level permissions must be contents: read"
        );
    }
}

/// The text of one top-level job (`  <job>:` up to the next job).
fn job<'a>(text: &'a str, job: &str) -> &'a str {
    let start = text
        .find(&format!("\n  {job}:\n"))
        .unwrap_or_else(|| panic!("no job {job}"));
    let body = &text[start + 1..];
    let end = body[1..]
        .match_indices("\n  ")
        .find(|(i, _)| {
            let after = &body[1 + i + 3..];
            after.starts_with(|c: char| c.is_ascii_alphabetic())
        })
        .map_or(body.len(), |(i, _)| i + 1);
    &body[..end]
}

#[test]
fn only_the_release_job_can_attest_and_it_does() {
    for (name, text) in workflows() {
        if name != "release.yml" {
            assert!(!text.contains("id-token"), "{name}");
            assert!(!text.contains("attestations:"), "{name}");
            continue;
        }
        let release = job(&text, "release");
        assert!(release.contains("id-token: write"), "{release}");
        assert!(release.contains("attestations: write"), "{release}");
        assert!(release.contains("contents: write"), "{release}");
        assert!(
            release.contains("actions/attest-build-provenance@"),
            "{release}"
        );
        assert!(release.contains("subject-path:"), "{release}");
        // The attestation comes before the release is published.
        assert!(
            release.find("attest-build-provenance").unwrap()
                < release.find("gh release create").unwrap()
        );
        for other in ["check", "build"] {
            let j = job(&text, other);
            assert!(!j.contains("id-token"), "{other}");
            assert!(!j.contains("attestations"), "{other}");
            assert!(!j.contains("contents: write"), "{other}");
        }
        assert_eq!(text.matches("id-token: write").count(), 1);
    }
}
