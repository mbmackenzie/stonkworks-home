use esp_idf_svc::http::{
    Method,
    client::{Configuration, EspHttpConnection, FollowRedirectsPolicy},
};
use sonos_desk_firmware::{config::Config, frame::MAX_PNG_BYTES};
use std::time::{Duration, Instant};

pub struct Response {
    pub png: Option<Vec<u8>>,
    pub etag: Option<String>,
}

/// Use the network task's existing connection; never change Wi-Fi state here.
pub fn fetch_connected(config: &Config, etag: Option<&str>) -> anyhow::Result<Response> {
    let base = config
        .http_base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set http_base_url"))?;
    let room = config
        .room_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set room_id"))?;
    // IDs are validated URL-safe characters; no credentials or assignments in logs.
    let url = format!("{}/frame?room={room}&mode=mono", base.trim_end_matches('/'));
    let mut http = EspHttpConnection::new(&Configuration {
        timeout: Some(Duration::from_secs(5)),
        follow_redirects_policy: FollowRedirectsPolicy::FollowNone,
        buffer_size: Some(1024),
        ..Default::default()
    })?;
    let mut headers = vec![("Accept", "image/png")];
    if let Some(tag) = etag {
        headers.push(("If-None-Match", tag));
    }
    http.initiate_request(Method::Get, &url, &headers)?;
    http.initiate_response()?;
    if http.status() == 304 {
        anyhow::ensure!(etag.is_some(), "Unexpected HTTP 304");
        return Ok(Response {
            png: None,
            etag: None,
        });
    }
    let response_etag = http
        .header("ETag")
        .filter(|tag| {
            tag.len() <= 128 && tag.is_ascii() && !tag.bytes().any(|b| b < 32 || b == 127)
        })
        .map(str::to_owned);
    anyhow::ensure!(
        http.status() == 200,
        "Frame service returned HTTP {}",
        http.status()
    );
    anyhow::ensure!(
        http.header("Content-Type").is_some_and(|h| h
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("image/png")),
        "Frame response is not PNG"
    );
    let length = http
        .header("Content-Length")
        .map(|n| n.parse::<usize>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("Invalid frame length"))?;
    anyhow::ensure!(
        length.is_none_or(|n| n > 0 && n <= MAX_PNG_BYTES),
        "Frame response exceeds limit"
    );
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length.unwrap_or(MAX_PNG_BYTES))
        .map_err(|_| anyhow::anyhow!("Not enough memory for frame response"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut chunk = [0_u8; 2048];
    loop {
        anyhow::ensure!(Instant::now() < deadline, "Frame download timed out");
        let count = http.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        anyhow::ensure!(
            bytes.len() + count <= MAX_PNG_BYTES && length.is_none_or(|n| bytes.len() + count <= n),
            "Frame response exceeds limit"
        );
        bytes.extend_from_slice(&chunk[..count]);
    }
    anyhow::ensure!(
        length.is_none_or(|n| n == bytes.len()),
        "Truncated frame response"
    );
    bytes.shrink_to_fit();
    println!("HTTP: received {} PNG bytes", bytes.len());
    Ok(Response {
        png: Some(bytes),
        etag: response_etag,
    })
}
