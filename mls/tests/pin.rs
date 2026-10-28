//! openmls is unaudited and has had High advisories (ADR 0025 section 2): it is pinned exactly, and
//! moving it is a deliberate chore with `mls/advisories-reviewed.txt` read first. This fails if the
//! pins loosen, or if the lock file holds a version other than the one reviewed here.

/// The openmls version whose advisories were last read (see `mls/advisories-reviewed.txt`).
const REVIEWED: &str = "0.9.2";

fn read(rel: &str) -> String {
    std::fs::read_to_string(format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn every_openmls_crate_is_pinned_exactly() {
    let manifest = read("Cargo.toml");
    let mut seen = 0;
    for line in manifest.lines() {
        let l = line.trim_start();
        if l.starts_with("openmls") || l.starts_with("tls_codec") {
            seen += 1;
            assert!(l.contains("\"="), "not an exact pin: {l}");
        }
    }
    assert!(
        seen >= 6,
        "expected the openmls crates and tls_codec, found {seen}"
    );
}

#[test]
fn the_lock_file_holds_the_reviewed_version() {
    // a Windows checkout may have turned the line endings into CRLF
    let lock = read("../Cargo.lock").replace("\r\n", "\n");
    let at = lock
        .find("name = \"openmls\"\n")
        .expect("openmls in the lock file");
    let version = lock[at..].lines().nth(1).unwrap();
    assert_eq!(
        version,
        format!("version = \"{REVIEWED}\""),
        "openmls moved: read the advisories (scripts/openmls_advisories.py), update mls/advisories-reviewed.txt and REVIEWED here"
    );
}
