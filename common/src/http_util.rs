use std::{
    error::Error,
    fmt,
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use http::{HeaderValue, Method, StatusCode};

#[derive(Debug)]
pub struct AutonomiHttpStatusError {
    pub method: Method,
    pub path: String,
    pub status: StatusCode,
    pub body: String,
}

impl fmt::Display for AutonomiHttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} failed: {} {}",
            self.method, self.path, self.status, self.body
        )
    }
}

impl Error for AutonomiHttpStatusError {}

pub fn run_http_healthcheck(addr: &str, path: &str) -> anyhow::Result<()> {
    validate_healthcheck_input("addr", addr)?;
    validate_healthcheck_input("path", path)?;
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let timeout = Duration::from_secs(5);
    let mut stream = TcpStream::connect(addr)
        .map_err(|err| anyhow::anyhow!("healthcheck could not connect to {addr}: {err}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| anyhow::anyhow!("healthcheck could not set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| anyhow::anyhow!("healthcheck could not set write timeout: {err}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|err| anyhow::anyhow!("healthcheck could not write request: {err}"))?;

    let mut response = Vec::with_capacity(128);
    let mut buffer = [0_u8; 64];
    while response.len() < 256 {
        let remaining = 256 - response.len();
        let read_len = remaining.min(buffer.len());
        let read = stream
            .read(&mut buffer[..read_len])
            .map_err(|err| anyhow::anyhow!("healthcheck could not read response: {err}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.windows(2).any(|window| window == b"\r\n") {
            break;
        }
    }

    let status_line_end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .ok_or_else(|| anyhow::anyhow!("healthcheck response did not include a status line"))?;
    let status_line = std::str::from_utf8(&response[..status_line_end])
        .map_err(|err| anyhow::anyhow!("healthcheck status line was not UTF-8: {err}"))?;
    let status = status_line.split_whitespace().nth(1).unwrap_or("");
    if status.starts_with('2') {
        return Ok(());
    }
    anyhow::bail!("healthcheck {addr}{path} returned {status_line:?}");
}

fn validate_healthcheck_input(name: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("healthcheck {name} must not be empty");
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        anyhow::bail!("healthcheck {name} must not contain whitespace or control characters");
    }
    Ok(())
}

pub fn run_healthcheck_from_args<I, S>(args: I) -> anyhow::Result<bool>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();
    let Some(command) = args.next() else {
        return Ok(false);
    };
    if command != "--healthcheck" {
        return Ok(false);
    }
    let addr = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: --healthcheck <addr> [path]"))?;
    let path = args.next().unwrap_or_else(|| "/livez".to_string());
    if args.next().is_some() {
        anyhow::bail!("usage: --healthcheck <addr> [path]");
    }
    run_http_healthcheck(&addr, &path)?;
    Ok(true)
}

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

    #[test]
    fn healthcheck_from_args_probes_http_livez() {
        let addr = spawn_healthcheck_server(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]);

        assert!(
            run_healthcheck_from_args(["service", "--healthcheck", addr.as_str(), "/livez",])
                .unwrap()
        );
    }

    #[test]
    fn healthcheck_handles_fragmented_status_line() {
        let addr =
            spawn_healthcheck_server(vec![b"HTTP/1.1", b" 200 OK\r\nContent-Length: 0\r\n\r\n"]);

        run_http_healthcheck(&addr, "/livez").unwrap();
    }

    #[test]
    fn healthcheck_fails_on_server_error() {
        let addr = spawn_healthcheck_server(vec![
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
        ]);

        let err = run_http_healthcheck(&addr, "/livez").unwrap_err();
        assert!(err.to_string().contains("503 Service Unavailable"));
    }

    #[test]
    fn healthcheck_fails_on_connection_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);

        let err = run_http_healthcheck(&addr, "/livez").unwrap_err();
        assert!(err.to_string().contains("could not connect"));
    }

    #[test]
    fn healthcheck_rejects_header_injection_inputs() {
        let err = run_healthcheck_from_args([
            "service",
            "--healthcheck",
            "127.0.0.1:80",
            "/livez\r\nX-Test: injected",
        ])
        .unwrap_err();

        assert!(err.to_string().contains("whitespace or control characters"));
    }

    fn spawn_healthcheck_server(chunks: Vec<&'static [u8]>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 256];
            let _ = stream.read(&mut request).unwrap();
            for chunk in chunks {
                stream.write_all(chunk).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        addr
    }
}
