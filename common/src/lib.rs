use http::HeaderValue;

pub fn normalize_cors_origin(origin: &str) -> anyhow::Result<String> {
    let origin = origin.trim().trim_end_matches('/');
    if origin == "*" {
        anyhow::bail!("CORS_ALLOWED_ORIGINS must list explicit origins, not '*'.");
    }

    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| {
            anyhow::anyhow!("CORS_ALLOWED_ORIGINS entries must start with http:// or https://")
        })?;

    if host.is_empty() || host.contains('/') || host.contains('?') || host.contains('#') {
        anyhow::bail!(
            "CORS_ALLOWED_ORIGINS entries must be origins like 'https://example.com' with no path, query, or wildcard."
        );
    }

    Ok(origin.to_string())
}

pub fn parse_cors_allowed_origins(raw_origins: &str) -> anyhow::Result<Vec<HeaderValue>> {
    raw_origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let origin = normalize_cors_origin(origin)?;
            HeaderValue::from_str(&origin)
                .map_err(|err| anyhow::anyhow!("invalid CORS origin '{}': {}", origin, err))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origin_normalization_accepts_explicit_origins() {
        assert_eq!(
            normalize_cors_origin(" https://example.com/ ").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_cors_origin("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn cors_origin_normalization_rejects_wildcards_paths_and_missing_schemes() {
        assert!(normalize_cors_origin("*").is_err());
        assert!(normalize_cors_origin("https://example.com/app").is_err());
        assert!(normalize_cors_origin("example.com").is_err());
    }

    #[test]
    fn parses_comma_separated_allowed_origins() {
        let origins = parse_cors_allowed_origins("http://localhost, https://example.com/").unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "http://localhost");
        assert_eq!(origins[1], "https://example.com");
    }
}
