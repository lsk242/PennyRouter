//! Iter 46: sync fetch health stamps per group; failures leave the
//! previous stamp (or absence) in place, so health degrades
//! instead of lying.

use pennyrouter_catalog::{read_fetch_health, record_fetch_health, Catalog, FETCH_SOURCES};

#[test]
fn record_read_round_trip_per_group() {
    let dir = std::env::temp_dir().join(format!("fetch-health-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = Catalog::open(&dir.join("t.db")).unwrap();
    catalog.init().unwrap();
    assert!(read_fetch_health(&catalog).is_empty());
    record_fetch_health(&catalog, "aa").unwrap();
    let health = read_fetch_health(&catalog);
    assert_eq!(health.len(), 1);
    assert!(health["aa"].contains('T'), "RFC3339 stamp");
    record_fetch_health(&catalog, "popularity").unwrap();
    let health = read_fetch_health(&catalog);
    assert_eq!(health.len(), 2);
    // Re-recording refreshes one group without touching the other.
    record_fetch_health(&catalog, "aa").unwrap();
    assert_eq!(read_fetch_health(&catalog).len(), 2);
    // Malformed health reads as empty, never an error.
    catalog.set_meta("fetch_health", "junk").unwrap();
    assert!(read_fetch_health(&catalog).is_empty());
    // The known groups are exactly what the sync records.
    assert_eq!(FETCH_SOURCES, &["aa", "popularity", "benchmarks", "apps"]);
}
