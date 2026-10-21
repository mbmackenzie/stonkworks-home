use esp_idf_svc::http::{
    Method,
    client::{Configuration, EspHttpConnection, FollowRedirectsPolicy},
};
use sonos_desk_firmware::{
    config::Config,
    frame::{BitmapError, check_frame_content_type, fill_bitmap},
};
use std::time::{Duration, Instant};

pub enum Body {
    /// HTTP 304: keep the current display.
    Unchanged,
    /// The packed frame was read straight into the caller's buffer.
    Bitmap,
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
    let mut headers = vec![("Accept", "application/octet-stream")];
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
    check_frame_content_type(http.header("Content-Type"))
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    let length = http
        .header("Content-Length")
        .map(|n| n.parse::<usize>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("Invalid frame length"))?;
    anyhow::ensure!(
        length.is_none_or(|n| n == bitmap.len()),
        "Bitmap response is not one frame"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
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
    Ok(Response {
        body: Body::Bitmap,
        etag: response_etag,
    })
}
