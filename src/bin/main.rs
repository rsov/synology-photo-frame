#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::vec;
use alloc::vec::Vec;
use defmt::{error, info, println};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_hal_bus::spi::ExclusiveDevice;
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
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use {esp_backtrace as _, esp_println as _};
extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

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

    let image_bytes = get_image_data(net_stack).await;

    let cursor = ZCursor::new(image_bytes);
    let mut decoder = JpegDecoder::new(cursor);

    let mut pixels = decoder.decode().expect("is fucked");
    let img_info = decoder.info().expect("Missing JPEG info");

    let dithered_bytes = floyd_steinberg_dither(img_info.width.into(), &mut pixels);

    // I think HexColor should be embedded_graphics_core::pixelcolor::raw::RawU4 because it causes this weird bug
    // image size: 600x338
    // embedded grahics size: 600x676
    println!("image size: {:?}x{:?}", img_info.width, img_info.height);

    let raw = ImageRaw::<HexColor>::new(&dithered_bytes, (img_info.width) as u32);
    let r = raw.bounding_box().size;
    println!("raw size {:?}x{:?}", r.width, r.height);

    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        esp_hal::spi::master::Config::default()
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(esp_hal::spi::Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO7)
    .with_mosi(peripherals.GPIO8)
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

static NETWORK_RESOURCES: static_cell::ConstStaticCell<embassy_net::StackResources<4>> =
    static_cell::ConstStaticCell::new(embassy_net::StackResources::new());

static RADIO_CONTROLLER: static_cell::StaticCell<esp_radio::Controller> =
    static_cell::StaticCell::new();

use embedded_io_async::BufRead;
async fn get_image_data<'t>(stack: embassy_net::Stack<'t>) -> alloc::vec::Vec<u8> {
    // DNS Client
    let dns = embassy_net::dns::DnsSocket::new(stack);

    // TCP state
    let tcp_state = embassy_net::tcp::client::TcpClientState::<1, 4096, 4096>::new();
    let tcp = embassy_net::tcp::client::TcpClient::new(stack, &tcp_state);

    println!("Attempting to do HTTP request");

    let mut write_buffer = [0u8; 4096];
    let mut read_buffer = [0u8; 16640];
    let config = TlsConfig::new(
        69420,
        &mut read_buffer,
        &mut write_buffer,
        reqwless::client::TlsVerify::None,
    );

    let mut http_client = reqwless::client::HttpClient::new_with_tls(&tcp, &dns, config);

    // const URL: &str = env!("WIFI_URL");
    // let url = "https://makeameme.org/media/templates/mocking-spongebob.jpg";";
    // let url = "https://makeameme.org/media/templates/happy_homer.jpg";
    let url = "https://makeameme.org/media/templates/upvote_obama.jpg";
    // let url = "https://upload.wikimedia.org/wikipedia/commons/thumb/5/5c/Double-alaskan-rainbow.jpg/500px-Double-alaskan-rainbow.jpg";

    let mut request = http_client
        .request(reqwless::request::Method::GET, url)
        .await
        .unwrap()
        .headers(&[("User-Agent", "ESP32S3")]);

    println!("HTTP request done?");

    let mut http_rx_buf = [0u8; 4096];
    let response = request.send(&mut http_rx_buf).await.unwrap();
    let status = response.status.clone();

    let mut body = response.body().reader();
    println!("Reading body");

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
    println!("Got body");

    if !status.is_successful() {
        error!("{:?}", core::str::from_utf8(&data).unwrap());
    }

    data
}

// This bit was Ai generated. Could implement better buffer handling
pub fn floyd_steinberg_dither(width: usize, src: &[u8]) -> Vec<u8> {
    let height = src.len() / (width * 3);
    assert_eq!(src.len(), width * height * 3);

    let mut work: Vec<[f32; 3]> = src
        .chunks_exact(3)
        .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
        .collect();

    let mut out = vec![0u8; width * height];

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;

            let old = work[idx];

            // Convert to Rgb888 for your existing From<Rgb888> → HexColor
            let rgb = Rgb888::new(
                old[0].clamp(0.0, 255.0) as u8,
                old[1].clamp(0.0, 255.0) as u8,
                old[2].clamp(0.0, 255.0) as u8,
            );

            let new_color = HexColor::from(rgb);
            out[idx] = new_color.get_nibble();

            // Quantized RGB from your palette
            let (qr, qg, qb) = new_color.rgb();
            let quant = [qr as f32, qg as f32, qb as f32];

            // Error = original - quantized
            let err = [old[0] - quant[0], old[1] - quant[1], old[2] - quant[2]];

            // Floyd–Steinberg diffusion
            //       *   7/16
            //  3/16 5/16 1/16

            // Right
            if x + 1 < width {
                let i = idx + 1;
                for c in 0..3 {
                    work[i][c] += err[c] * 7.0 / 16.0;
                }
            }

            // Bottom row
            if y + 1 < height {
                // Bottom-left
                if x > 0 {
                    let i = idx + width - 1;
                    for c in 0..3 {
                        work[i][c] += err[c] * 3.0 / 16.0;
                    }
                }

                // Bottom
                {
                    let i = idx + width;
                    for c in 0..3 {
                        work[i][c] += err[c] * 5.0 / 16.0;
                    }
                }

                // Bottom-right
                if x + 1 < width {
                    let i = idx + width + 1;
                    for c in 0..3 {
                        work[i][c] += err[c] * 1.0 / 16.0;
                    }
                }
            }
        }
    }

    out
}
