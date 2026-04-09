#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::boxed::Box;
use alloc::format;
use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::iso_8859_14::FONT_10X20;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::text::{Alignment, Text};
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
use synology_photo_frame::battery::get_charge_state;
use synology_photo_frame::images::{floyd_steinberg_dither, mitchell_upscale};
use synology_photo_frame::synology::get_image;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use {esp_backtrace as _, esp_println as _};
extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

static NETWORK_RESOURCES: static_cell::ConstStaticCell<embassy_net::StackResources<3>> =
    static_cell::ConstStaticCell::new(embassy_net::StackResources::new());

static RADIO_CONTROLLER: static_cell::StaticCell<esp_radio::Controller> =
    static_cell::StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    info!("BOOTING");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let mut gpio_btn_reset = peripherals.GPIO3;

    let mut rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    esp_hal::gpio::Input::new(
        gpio_btn_reset.reborrow(),
        esp_hal::gpio::InputConfig::default().with_pull(Pull::Up),
    )
    .is_low();

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    let charge_state = get_charge_state(peripherals.ADC1, peripherals.GPIO1, peripherals.GPIO21);

    let epd_spi_bus = Spi::new(
        peripherals.SPI2,
        esp_hal::spi::master::Config::default()
            .with_frequency(esp_hal::time::Rate::from_mhz(20))
            .with_mode(esp_hal::spi::Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO7)
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

    let mut epd7in3e = Box::new(
        Epd7in3e::new(
            &mut epd_spi_dev,
            screen_busy,
            screen_dc,
            screen_rst,
            &mut delay,
            None,
        )
        .unwrap(),
    );

    let mut display = Box::new(Display7in3e::default());

    // Prevent battery damage
    if charge_state.percent == 0 {
        Text::with_alignment(
            format!(
                "I NEEDS A CHARGE\nBATTERY IS {}% v{:.2}\nPRESS RESET TO UPDATE",
                charge_state.percent, charge_state.volts
            )
            .as_str(),
            Point::new(
                (epd7in3e.width() / 2) as i32,
                (epd7in3e.height() / 2) as i32,
            ),
            MonoTextStyle::new(&FONT_10X20, HexColor::White),
            Alignment::Center,
        )
        .draw(display.as_mut())
        .unwrap();

        epd7in3e
            .update_and_display_frame(&mut epd_spi_dev, display.buffer(), &mut delay)
            .unwrap();

        epd7in3e.sleep(&mut epd_spi_dev, &mut delay).unwrap();

        let wakeup_pins: &mut [(
            &mut dyn esp_hal::gpio::RtcPin,
            esp_hal::rtc_cntl::sleep::WakeupLevel,
        )] = &mut [(
            &mut gpio_btn_reset,
            esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
        )];
        let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

        info!("[BAT] -> Going for long sleep");
        rtc.sleep_deep(&[&pin_wake_source]);
    }

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

    info!("[NET] Waiting for network link...");
    net_stack.wait_link_up().await;
    info!("[NET] Link up, waiting for config up");
    net_stack.wait_config_up().await;
    info!("[NET] Network config up! {:?}", net_stack.config_v4());

    const SYN_BASE: &str = env!("SYN_BASE");
    const SYN_USER: &str = env!("SYN_USER");
    const SYN_PASS: &str = env!("SYN_PASS");
    const SYN_ALBUM: &str = env!("SYN_ALBUM");
    // THIS HAS TO BE DONE ASAP BECAUSE THERE'S SOME BULLSH*T BEHAVIOR IF THE STACK SIZE IS OVER 50% AND IT TRIES TO MAKE A COPY OF IT FOR SOME DUMB ASS REASON
    let image_bytes = get_image(net_stack, SYN_BASE, SYN_USER, SYN_PASS, SYN_ALBUM).await;

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
    // embedded graphics size: 600x676
    info!(
        "[PIC] image size: {:?}x{:?}",
        img_info.width, img_info.height
    );

    // Not sure of boxing the image will do anything as it already takes in a vac but fuck it, we ball
    let mut raw = Box::new(ImageRaw::<HexColor>::new(
        &dithered_bytes,
        (resized_width) as u32,
    ));
    let r = raw.bounding_box().size;
    info!("[PIC] raw size {:?}x{:?}", r.width, r.height);

    let size = display.size();
    let center = Point::new((size.width as i32) / 2, (size.height as i32) / 2);

    let image = Box::new(Image::with_center(raw.as_mut(), center));

    let s = image.bounding_box().size;
    info!("[PIC] Embedded image size: {:?}x{:?}", s.width, s.height);

    image.draw(display.as_mut()).unwrap();

    Rectangle::new(
        Point::new(
            // Make the box smaller if we don't need the whole numbers
            if charge_state.percent == 100 {
                740
            } else {
                770
            },
            0,
        ),
        Size::new(800, 20),
    )
    .into_styled(
        PrimitiveStyleBuilder::new()
            .fill_color(if charge_state.percent <= 10 {
                HexColor::Red
            } else {
                HexColor::Black
            })
            .build(),
    )
    .draw(display.as_mut())
    .unwrap();

    Text::with_alignment(
        format!("{:?}%", charge_state.percent).as_str(),
        Point::new(795, 15),
        MonoTextStyle::new(&FONT_10X20, HexColor::White),
        Alignment::Right,
    )
    .draw(display.as_mut())
    .unwrap();

    epd7in3e
        .update_and_display_frame(&mut epd_spi_dev, display.buffer(), &mut delay)
        .unwrap();

    epd7in3e.sleep(&mut epd_spi_dev, &mut delay).unwrap();

    let wakeup_pins: &mut [(
        &mut dyn esp_hal::gpio::RtcPin,
        esp_hal::rtc_cntl::sleep::WakeupLevel,
    )] = &mut [(
        &mut gpio_btn_reset,
        esp_hal::rtc_cntl::sleep::WakeupLevel::Low,
    )];
    let pin_wake_source = esp_hal::rtc_cntl::sleep::RtcioWakeupSource::new(wakeup_pins);

    let timer_wake_source =
        esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(core::time::Duration::from_hours(9));
    let wake_sources: &[&dyn esp_hal::rtc_cntl::sleep::WakeSource] =
        &[&timer_wake_source, &pin_wake_source];

    info!("[ESP] Going to deep sleep :)");
    rtc.sleep_deep(wake_sources);
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    info!("[NET] Start connection task");
    info!("[NET] Device capabilities: {:?}", controller.capabilities());

    info!("[NET] Starting WiFi");
    controller.start_async().await.unwrap();
    info!("[NET] Wifi started");
    loop {
        info!("[NET] Connecting WiFi");
        match controller.connect_async().await {
            Ok(_) => {
                info!("[NET] Connected");
                controller
                    .wait_for_event(esp_radio::wifi::WifiEvent::StaDisconnected)
                    .await;
                info!("[NET] Disconnected");
            }
            Err(e) => {
                info!("[NET] Failed to connect to wifi: {:?}", e);
                info!("[NET] Retry in 5sec");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}
