use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use defmt::{error, info, println};
use embedded_io_async::BufRead;
use reqwless::client::TlsConfig;
use reqwless::request::RequestBuilder;
use serde::Deserialize;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

#[derive(Deserialize, Debug)]
struct GetThumbnailParams {
    id: i64,
    cache_key: String,
}

pub async fn get_image<'t>(
    stack: embassy_net::Stack<'t>,
    base: &str,
    user: &str,
    pass: &str,
    album_passphrase: &str,
) -> alloc::vec::Vec<u8> {
    let dns = embassy_net::dns::DnsSocket::new(stack);
    let tcp_state = Box::new(embassy_net::tcp::client::TcpClientState::<1, 2048, 2048>::new());

    let tcp = embassy_net::tcp::client::TcpClient::new(stack, &tcp_state);

    let mut write_buffer = alloc::vec![0u8; 2048];
    let mut read_buffer = alloc::vec![0u8; 16640];
    let config = TlsConfig::new(
        696969,
        &mut read_buffer,
        &mut write_buffer,
        reqwless::client::TlsVerify::None,
    );

    let mut http_client = reqwless::client::HttpClient::new_with_tls(&tcp, &dns, config);

    info!("[HTTP] Ready");

    // First request: Authentication
    let sid = {
        let url = url::Url::parse_with_params(
            format!("{}/webapi/entry.cgi", base).as_str(),
            &[
                ("api", "SYNO.API.Auth"),
                ("version", "6"),
                ("method", "login"),
                ("format", "sid"),
                ("account", user),
                ("passwd", pass),
            ],
        )
        .unwrap();

        info!("[HTTP] -> {}", url.as_str());
        info!("[HTTP] -> {}", esp_alloc::HEAP.stats());

        let request_builder = http_client
            .request(reqwless::request::Method::GET, &url.as_str())
            .await;

        if let Err(e) = request_builder {
            error!("Failed to build HTTP request: {:?}", e);
            return alloc::vec::Vec::new();
        }

        let mut request = request_builder.unwrap();

        info!("[HTTP] Getting auth token");

        let mut http_rx_buf = alloc::vec![0u8; 4096];
        let response = request.send(&mut http_rx_buf).await.unwrap();
        let status = response.status.clone();

        let mut body = response.body().reader();
        info!("[HTTP] Reading auth body");

        let mut data = alloc::vec::Vec::new();
        loop {
            let chunk = body.fill_buf().await.unwrap();
            if chunk.is_empty() {
                break;
            }

            data.extend_from_slice(chunk);
            let len = chunk.len();
            body.consume(len);
        }
        info!(
            "[HTTP] Got auth body {:?}",
            core::str::from_utf8(&data).unwrap()
        );

        if !status.is_successful() {
            error!("{:?}", core::str::from_utf8(&data).unwrap());
            return alloc::vec::Vec::new();
        }

        let stuff: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let sid = stuff["data"]["sid"].as_str().unwrap().to_owned();
        info!("[HTTP] Auth SID: {:?}", sid.as_str());

        sid.clone()
    };

    // Second request: List album items
    let thumb_params: GetThumbnailParams = {
        let url = url::Url::parse_with_params(
            format!("{}/webapi/entry.cgi/SYNO.Foto.Browse.Item", base).as_str(),
            &[
                ("api", "SYNO.Foto.Browse.Item"),
                ("version", "4"),
                ("method", "list"),
                ("additional", "[\"thumbnail\"]"),
                ("sort_by", "takentime"),
                ("offset", "0"), // TODO: Use these to retrieve just the one random
                ("limit", "64"),
                ("sort_direction", "asc"),
                ("passphrase", album_passphrase),
                ("_sid", &sid),
            ],
        )
        .unwrap();

        info!("[URL] -> {}", url.as_str());

        let request_builder = http_client
            .request(reqwless::request::Method::GET, &url.as_str())
            .await;

        if let Err(e) = request_builder {
            error!("Failed to build HTTP request list album: {:?}", e);
            return alloc::vec::Vec::new();
        }

        let mut request = request_builder.unwrap();

        let mut http_rx_buf = alloc::vec![0u8; 4096];
        let response = request.send(&mut http_rx_buf).await.unwrap();
        let status = response.status.clone();

        let mut body = response.body().reader();
        println!("Reading album body");

        let mut data = alloc::vec::Vec::new();
        loop {
            let chunk = body.fill_buf().await.unwrap();
            if chunk.is_empty() {
                break;
            }

            data.extend_from_slice(chunk);
            let len = chunk.len();
            body.consume(len);
        }
        println!("Got album body {:?}", core::str::from_utf8(&data).unwrap());

        if !status.is_successful() {
            error!("{:?}", core::str::from_utf8(&data).unwrap());
        }

        let stuff: serde_json::Value = serde_json::from_slice(&data).unwrap();

        let album_list = stuff["data"]["list"].as_array().unwrap();

        let rand = esp_hal::rng::Rng::new().random();
        let rand_index = if album_list.is_empty() {
            0
        } else {
            (rand as usize) % album_list.len() as usize
        };

        let photo_object = album_list.get(rand_index).unwrap();

        let cache_key = photo_object["additional"]["thumbnail"]["cache_key"]
            .as_str()
            .unwrap()
            .to_string();

        let id = photo_object["id"].as_i64().unwrap();

        println!("cache key {}", cache_key.as_str());
        GetThumbnailParams {
            id: id,
            cache_key: cache_key,
        }
    };

    {
        let url = url::Url::parse_with_params(
            format!("{}/synofoto/api/v2/t/Thumbnail/get", base).as_str(),
            &[
                ("api", "SYNO.Foto.Thumbnail"),
                ("version", "1"),
                ("method", "get"),
                ("mode", "download"),
                ("id", thumb_params.id.to_string().as_str()),
                ("type", "unit"),
                ("size", "m"),
                ("passphrase", album_passphrase),
                ("cache_key", &thumb_params.cache_key),
                ("_sid", &sid),
            ],
        )
        .unwrap();

        info!("[URL] -> {}", url.as_str());

        let request_builder = http_client
            .request(reqwless::request::Method::GET, &url.as_str())
            .await;

        if let Err(e) = request_builder {
            error!("Failed to build HTTP request list album: {:?}", e);
            return Vec::new();
        }

        let mut request = request_builder
            .unwrap()
            .headers(&[("User-Agent", "ESP32S3")]);

        let mut http_rx_buf = alloc::vec![0u8; 4096];
        let response = request.send(&mut http_rx_buf).await.unwrap();
        let status = response.status.clone();

        let mut body = response.body().reader();
        println!("Reading thumbnail body");

        let mut data = Vec::new();
        loop {
            let chunk = body.fill_buf().await.unwrap();
            if chunk.is_empty() {
                break;
            }

            data.extend_from_slice(chunk);
            let len = chunk.len();
            body.consume(len);
        }

        if !status.is_successful() {
            error!("{:?}", core::str::from_utf8(&data).unwrap());
        }

        data
    }
}
