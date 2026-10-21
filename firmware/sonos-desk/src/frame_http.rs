use esp_idf_svc::{
    hal::modem::Modem,
    http::{
        Method,
        client::{Configuration, EspHttpConnection, FollowRedirectsPolicy},
    },
};
use sonos_desk_firmware::{config::Config, frame::MAX_PNG_BYTES};
use std::time::{Duration, Instant};

pub fn fetch(modem: Modem<'static>, config: &Config) -> anyhow::Result<Vec<u8>> {
    let base = config
        .http_base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set http_base_url in local configuration"))?;
    let room = config
        .room_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Set room_id in local configuration"))?;
    let mut wifi = crate::wifi::start(modem, &config.wifi)?;
    println!("Wi-Fi: connecting for one frame request");
    wifi.connect()?;
    wifi.wait_netif_up()?;
    println!("Wi-Fi: connected; fetching selected room's mono frame");
    // IDs are validated URL-safe characters; no credentials or assignments in logs.
    let url = format!("{}/frame?room={room}&mode=mono", base.trim_end_matches('/'));
    let mut http = EspHttpConnection::new(&Configuration {
        timeout: Some(Duration::from_secs(5)),
        follow_redirects_policy: FollowRedirectsPolicy::FollowNone,
        buffer_size: Some(1024),
        ..Default::default()
    })?;
    http.initiate_request(Method::Get, &url, &[("Accept", "image/png")])?;
    http.initiate_response()?;
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
    println!(
        "HTTP: received {} PNG bytes; closing Wi-Fi before decode",
        bytes.len()
    );
    Ok(bytes) // HTTP and Wi-Fi drivers drop here, freeing their buffers before PNG decoding.
}
