#![allow(clippy::unwrap_used)]

#[test]
fn normalizes_cors_origin_without_paths_or_wildcards() {
    assert_eq!(
        autvid_common::normalize_cors_origin("http://localhost:5173/").unwrap(),
        "http://localhost:5173"
    );
    assert!(autvid_common::normalize_cors_origin("*").is_err());
    assert!(autvid_common::normalize_cors_origin("http://localhost/app").is_err());
}
