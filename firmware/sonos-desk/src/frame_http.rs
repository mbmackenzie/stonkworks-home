use esp_idf_svc::http::{
    Method,
    client::{Configuration, EspHttpConnection, FollowRedirectsPolicy},
};
use sonos_desk_firmware::{
    config::Config,
    frame::{BitmapError, MAX_PNG_BYTES, fill_bitmap},
};
use std::time::{Duration, Instant};

pub enum Body {
    /// HTTP 304: keep the current display.
    Unchanged,
    /// The packed frame was read straight into the caller's buffer.
    Bitmap,
    /// An older service sent a PNG instead; decode it.
    Png(Vec<u8>),
}

pub struct Response {
    pub body: Body,
    pub etag: Option<String>,
}

/// Use the network task's existing connection; never change Wi-Fi state here.
pub fn fetch_connected(
    config: &Config,
    etag: Option<&str>,
    bitmap: &mut [u8; 48_000],
) -> anyhow::Result<Response> {
    let base = config
        .http_base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set http_base_url"))?;
    let room = config
        .room_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set room_id"))?;
    // IDs are validated URL-safe characters; no credentials or assignments in logs.
    let url = format!(
        "{}/frame?room={room}&mode=mono&format=bitmap",
        base.trim_end_matches('/')
    );
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
            body: Body::Unchanged,
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
    let content_type = http
        .header("Content-Type")
        .and_then(|h| h.split(';').next())
        .map(|h| h.trim().to_ascii_lowercase());
    let is_bitmap = content_type.as_deref() == Some("application/octet-stream");
    anyhow::ensure!(
        is_bitmap || content_type.as_deref() == Some("image/png"),
        "Frame response is neither a bitmap nor a PNG"
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
    let deadline = Instant::now() + Duration::from_secs(20);
    if is_bitmap {
        anyhow::ensure!(
            length.is_none_or(|n| n == bitmap.len()),
            "Bitmap response is not one frame"
        );
        fill_bitmap(
            |buffer| {
                anyhow::ensure!(Instant::now() < deadline, "Frame download timed out");
                Ok(http.read(buffer)?)
            },
            bitmap,
        )
        .map_err(|error| match error {
            BitmapError::Read(error) => error,
            BitmapError::Size(message) => anyhow::anyhow!("{message}"),
        })?;
        println!("HTTP: received a {} byte bitmap", bitmap.len());
        return Ok(Response {
            body: Body::Bitmap,
            etag: response_etag,
        });
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length.unwrap_or(MAX_PNG_BYTES))
        .map_err(|_| anyhow::anyhow!("Not enough memory for frame response"))?;
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
        body: Body::Png(bytes),
        etag: response_etag,
    })
}
