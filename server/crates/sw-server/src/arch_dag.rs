//! Machine-enforced crate dependency DAG.
//!
//! Parses every workspace member's `Cargo.toml` and asserts its intra-workspace
//! `[dependencies]` are a subset of the edges the layered architecture permits.
//! The server is the only crate allowed to pull the libraries together; the
//! libraries fan out from `sw-contracts` and never reference each other or the
//! server. Runs under `cargo test --workspace` (it lives behind `#[cfg(test)]`
//! in the server binary), so a stray `use another_crate` that a new dependency
//! edge would require turns into a red test rather than silent layer erosion.
//!
//! TOML is scanned by hand (a section walk plus a key split) to avoid taking a
//! parser dependency for a job this small; because the check only ever looks at
//! dependency names that match a known workspace crate, imperfect parsing of
//! unrelated lines is harmless.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Absolute path to the workspace root (`server/`), derived from this crate's
/// manifest dir (`server/crates/sw-server`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("server/crates/sw-server has a server/ ancestor")
        .to_path_buf()
}

/// The intra-workspace library crates that may appear as a dependency edge.
/// The server binary (`sailwind-online-server`) is a leaf consumer and is never
/// depended upon, so it is not in this set.
fn workspace_libs() -> BTreeSet<String> {
    [
        "sw-contracts",
        "sw-net",
        "sw-world",
        "sw-econ",
        "sw-persist",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Allowed intra-workspace edges, keyed by crate directory name.
fn allowed_edges() -> BTreeMap<String, BTreeSet<String>> {
    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    let mut map = BTreeMap::new();
    map.insert("sw-contracts".to_string(), set(&[]));
    map.insert("sw-net".to_string(), set(&["sw-contracts"]));
    map.insert("sw-world".to_string(), set(&["sw-contracts"]));
    map.insert("sw-econ".to_string(), set(&["sw-contracts"]));
    map.insert("sw-persist".to_string(), set(&["sw-contracts"]));
    map.insert(
        "sw-server".to_string(),
        set(&[
            "sw-contracts",
            "sw-net",
            "sw-world",
            "sw-econ",
            "sw-persist",
        ]),
    );
    map
}

/// Extract the quoted `members = [...]` entries from the workspace `Cargo.toml`,
/// returning each member's directory name (`crates/sw-net` -> `sw-net`).
fn workspace_members(manifest: &str) -> Vec<String> {
    let start = manifest
        .find("members")
        .and_then(|i| manifest[i..].find('[').map(|j| i + j + 1))
        .expect("[workspace] members array");
    let end = start + manifest[start..].find(']').expect("members array closes");

    let mut members = Vec::new();
    let mut rest = &manifest[start..end];
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let close = after.find('"').expect("members entry closes its quote");
        let entry = &after[..close];
        let dir = entry.rsplit('/').next().unwrap_or(entry).to_string();
        members.push(dir);
        rest = &after[close + 1..];
    }
    members
}

/// Collect the dependency names declared in a crate manifest's `[dependencies]`
/// section (only that section — dev/build deps and other tables are ignored).
fn dependency_names(manifest: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut in_deps = false;

    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }

        // A dependency line is `name = ...` or `name.attr = ...`; take the crate
        // name (text before the first '=', then before any '.').
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let name = key.split('.').next().unwrap_or(key).trim();
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_dag_has_no_forbidden_edges() {
        let root = workspace_root();
        let workspace_manifest =
            fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");
        let members = workspace_members(&workspace_manifest);
        let libs = workspace_libs();
        let allowed = allowed_edges();

        assert!(!members.is_empty(), "discovered no workspace members");

        // Every member must have a declared policy, and every policy must map to a
        // real member — no crate escapes the DAG, no stale rule lingers.
        let member_set: BTreeSet<String> = members.iter().cloned().collect();
        let policy_set: BTreeSet<String> = allowed.keys().cloned().collect();
        assert_eq!(
            member_set, policy_set,
            "workspace members and DAG policy keys must match exactly"
        );

        let mut violations = Vec::new();
        for member in &members {
            let manifest = fs::read_to_string(root.join("crates").join(member).join("Cargo.toml"))
                .unwrap_or_else(|e| panic!("read Cargo.toml for {member}: {e}"));

            let intra: BTreeSet<String> = dependency_names(&manifest)
                .into_iter()
                .filter(|d| libs.contains(d))
                .collect();

            let permitted = &allowed[member];
            for dep in &intra {
                if !permitted.contains(dep) {
                    violations.push(format!(
                        "{member} depends on {dep}, which is outside its allowed set {permitted:?}"
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "forbidden crate edges:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn parser_extracts_known_edges() {
        // Guards the hand-rolled scan: the server manifest must yield exactly its
        // five library edges, so a passing DAG test can never be a parser that
        // silently found nothing.
        let root = workspace_root();
        let manifest = fs::read_to_string(root.join("crates").join("sw-server").join("Cargo.toml"))
            .expect("read sw-server Cargo.toml");
        let libs = workspace_libs();
        let intra: BTreeSet<String> = dependency_names(&manifest)
            .into_iter()
            .filter(|d| libs.contains(d))
            .collect();

        assert_eq!(
            intra, libs,
            "sw-server should depend on every library crate"
        );
    }

    #[test]
    fn forbidden_edge_is_detected() {
        // Proves the rule bites: a hand-built manifest with an illegal edge
        // (sw-net -> sw-world) must be rejected.
        let manifest = "[dependencies]\nsw-contracts.workspace = true\nsw-world.workspace = true\n";
        let libs = workspace_libs();
        let allowed: BTreeSet<String> = ["sw-contracts"].into_iter().map(String::from).collect();
        let intra: BTreeSet<String> = dependency_names(manifest)
            .into_iter()
            .filter(|d| libs.contains(d))
            .collect();

        assert!(
            intra.iter().any(|d| !allowed.contains(d)),
            "an out-of-policy edge must be visible to the check"
        );
    }
}
