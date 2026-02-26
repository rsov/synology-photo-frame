#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use defmt::{error, info, println};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::prelude::*;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_io_async::BufRead;
use epd_waveshare::color::HexColor;
use epd_waveshare::epd7in3e::{Display7in3e, Epd7in3e};
use epd_waveshare::prelude::WaveshareDisplay;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::Spi;
use esp_hal::timer::timg::TimerGroup;
use reqwless::client::TlsConfig;
use reqwless::request::RequestBuilder;
use serde::Deserialize;
use synology_photo_frame::images::floyd_steinberg_dither;
use synology_photo_frame::images::mitchell_upscale;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use {esp_backtrace as _, esp_println as _};
extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

static NETWORK_RESOURCES: static_cell::ConstStaticCell<embassy_net::StackResources<4>> =
    static_cell::ConstStaticCell::new(embassy_net::StackResources::new());

static RADIO_CONTROLLER: static_cell::StaticCell<esp_radio::Controller> =
    static_cell::StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    const SSID: &str = env!("WIFI_SSID");
    const PASSWORD: &str = env!("WIFI_PASSWORD");

    let wifi_sta_device = interfaces.sta;

    let sta_config = embassy_net::Config::dhcpv4(Default::default());

    let station_config = esp_radio::wifi::ModeConfig::Client(
        esp_radio::wifi::ClientConfig::default()
            .with_ssid(SSID.into())
            .with_password(PASSWORD.into()),
    );
    wifi_controller.set_config(&station_config).unwrap();

    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (net_stack, net_runner) =
        embassy_net::new(wifi_sta_device, sta_config, NETWORK_RESOURCES.take(), seed);

    spawner.spawn(wifi_task(wifi_controller)).unwrap();
    spawner.spawn(net_task(net_runner)).unwrap();

    println!("Waiting for network link...");
    net_stack.wait_link_up().await;
    println!("Link up, waiting for config up");
    net_stack.wait_config_up().await;
    println!("Network config up! {:?}", net_stack.config_v4());

    const SYN_BASE: &str = env!("SYN_BASE");
    const SYN_USER: &str = env!("SYN_USER");
    const SYN_PASS: &str = env!("SYN_PASS");
    const SYN_ALBUM: &str = env!("SYN_ALBUM");
    // THIS HAS TO BE DONE ASAP BECAUSE THERE'S SOME BULLSH*T BEHAVIOUR IF THE STACK SIZE IS OVER 50% AND IT TRIES TO MAKE A COPY OF IT FOR SOME DUMB ASS REASON
    let image_bytes = get_stuff(net_stack, SYN_BASE, SYN_USER, SYN_PASS, SYN_ALBUM).await;

    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        esp_hal::spi::master::Config::default()
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(esp_hal::spi::Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO7)
    // .with_mosi(peripherals.GPIO8)
    .with_mosi(peripherals.GPIO9);

    info!("Bus ");
    let mut delay = Delay::new();

    let screen_cs = Output::new(peripherals.GPIO10, Level::Low, OutputConfig::default());
    let screen_dc = Output::new(peripherals.GPIO11, Level::Low, OutputConfig::default());
    let screen_rst = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let screen_busy = Input::new(
        peripherals.GPIO13,
        InputConfig::default().with_pull(Pull::Up),
    );

    let mut epd_spi_dev = ExclusiveDevice::new(epd_spi_bus, screen_cs, delay).unwrap();

    info!("Screen pins");

    let mut epd7in3e = Epd7in3e::new(
        &mut epd_spi_dev,
        screen_busy,
        screen_dc,
        screen_rst,
        &mut delay,
        None,
    )
    .unwrap();

    let mut display = Display7in3e::default();


    let cursor = ZCursor::new(image_bytes);
    let mut decoder = JpegDecoder::new(cursor);

    let pixels = decoder.decode().expect("is fucked");
    let img_info = decoder.info().expect("Missing JPEG info");

    let (resized, resized_width, _resized_height) = mitchell_upscale(
        pixels,
        img_info.width.into(),
        img_info.height.into(),
        epd7in3e.width() as usize,
        epd7in3e.height() as usize,
    );

    let dithered_bytes = floyd_steinberg_dither(resized_width.into(), resized);

    // I think HexColor should be embedded_graphics_core::pixelcolor::raw::RawU4 because it causes this weird bug
    // image size: 600x338
    // embedded grahics size: 600x676
    println!("image size: {:?}x{:?}", img_info.width, img_info.height);

    let raw = ImageRaw::<HexColor>::new(&dithered_bytes, (resized_width) as u32);
    let r = raw.bounding_box().size;
    println!("raw size {:?}x{:?}", r.width, r.height);

    let size = display.size();
    let center = Point::new((size.width as i32) / 2, (size.height as i32) / 2);

    let image = Image::with_center(&raw, center);

    let s = image.bounding_box().size;
    println!("Embedde image size: {:?}x{:?}", s.width, s.height);

    image.draw(&mut display).unwrap();

    epd7in3e
        .update_and_display_frame(&mut epd_spi_dev, display.buffer(), &mut delay)
        .unwrap();

    epd7in3e.sleep(&mut epd_spi_dev, &mut delay).unwrap();

    // let wakeup_pins: &mut [(
    //     &mut dyn esp_hal::gpio::RtcPin,
    //     esp_hal::rtc_cntl::sleep::WakeupLevel,
    // )] = &mut [(
    //     &mut gpio_btn_reset,
    //     esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
    // )];
    // let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

    // let timer_wake_source =
    //     esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(core::time::Duration::from_secs(10 * 60));
    // let wake_sources: &[&dyn esp_hal::rtc_cntl::sleep::WakeSource] =
    //     &[&timer_wake_source, &pin_wake_source];

    // println!("Going to deep sleep :)");
    // rtc.sleep_deep(wake_sources);

    loop {
        info!("Hello world!");
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    println!("Start connection task");
    println!("Device capabilities: {:?}", controller.capabilities());

    println!("Starting WiFi");
    controller.start_async().await.unwrap();
    println!("Wifi started");
    loop {
        println!("Connecting WiFi");
        match controller.connect_async().await {
            Ok(_) => {
                println!("Connected");
                controller
                    .wait_for_event(esp_radio::wifi::WifiEvent::StaDisconnected)
                    .await;
                println!("Disconnected");
            }
            Err(e) => {
                println!("Failed to connect to wifi: {:?}", e);
                println!("Retry in 5sec");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

#[derive(Deserialize, Debug)]
struct GetThumbnailParams {
    id: i64,
    cache_key: String,
}

async fn get_stuff<'t>(
    stack: embassy_net::Stack<'t>,
    base: &str,
    user: &str,
    pass: &str,
    album_passphase: &str,
) -> alloc::vec::Vec<u8> {
    let dns = embassy_net::dns::DnsSocket::new(stack);
    let tcp_state =
        alloc::boxed::Box::new(embassy_net::tcp::client::TcpClientState::<1, 2048, 2048>::new());

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

        let request_builder = http_client
            .request(reqwless::request::Method::GET, &url.as_str())
            .await;

        if let Err(e) = request_builder {
            error!("Failed to build HTTP request: {:?}", e);
            return alloc::vec::Vec::new();
        }

        let mut request = request_builder.unwrap();

        info!("[HTTP] Getting auth token");

        let mut http_rx_buf = [0u8; 4096];
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
                ("offset", "0"), // TODO: Use these to retreive just the one random
                ("limit", "64"),
                ("sort_direction", "asc"),
                ("passphrase", album_passphase),
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

        let mut http_rx_buf = [0u8; 4096];
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
                ("passphrase", album_passphase),
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
            return alloc::vec::Vec::new();
        }

        let mut request = request_builder
            .unwrap()
            .headers(&[("User-Agent", "ESP32S3")]);

        let mut http_rx_buf = [0u8; 4096];
        let response = request.send(&mut http_rx_buf).await.unwrap();
        let status = response.status.clone();

        let mut body = response.body().reader();
        println!("Reading thumbnail body");

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

        if !status.is_successful() {
            error!("{:?}", core::str::from_utf8(&data).unwrap());
        }

        data
    }
}
