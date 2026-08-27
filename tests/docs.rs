//! The README says what the code does.
//!
//! `keymap::BINDINGS` is the one table the `?` modal, the shortcuts bar, and
//! the README's key section are all written against. A binding added to the
//! code and not to the README — or left in the README after the code dropped
//! it — is the drift this test exists to catch. Nothing here opens a database
//! or draws a frame; it reads two files and compares them.

use std::path::Path;

use msgs::keymap::BINDINGS;

fn readme() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("read the README")
}

/// The `## Keys` section, up to the heading that follows it.
fn keys_section(readme: &str) -> &str {
    let start = readme.find("## Keys").expect("a Keys section");
    let rest = &readme[start + "## Keys".len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// The rows of every key table in the section: `(keys, action)` pairs, with the
/// table headers and their separator rules left out.
fn key_rows(section: &str) -> Vec<(String, String)> {
    section
        .lines()
        .filter(|line| line.starts_with("| `"))
        .filter_map(|line| {
            let mut cells = line.trim_matches('|').split('|');
            let keys = cells.next()?.trim().to_string();
            let action = cells.next()?.trim().to_string();
            Some((keys, action))
        })
        .collect()
}

#[test]
fn every_binding_in_the_code_is_in_the_readme_key_tables() {
    let readme = readme();
    let rows = key_rows(keys_section(&readme));
    for binding in BINDINGS {
        assert!(
            rows.iter().any(|(_, action)| action == binding.description),
            "the README key tables do not mention {:?} ({})",
            binding.keys,
            binding.description
        );
    }
}

#[test]
fn the_readme_key_tables_hold_nothing_the_code_does_not_bind() {
    let readme = readme();
    let rows = key_rows(keys_section(&readme));
    for (keys, action) in &rows {
        assert!(
            BINDINGS
                .iter()
                .any(|binding| binding.description == action.as_str()),
            "the README documents {keys:?} ({action}), which nothing binds"
        );
    }
    assert_eq!(
        rows.len(),
        BINDINGS.len(),
        "one README row per binding, no more and no fewer"
    );
}
