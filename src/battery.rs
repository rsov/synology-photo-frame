use defmt::info;
use embassy_time::{Duration, Timer};
use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcConfig, Attenuation},
    gpio::{AnalogPin, Level, Output, OutputConfig, OutputPin},
    peripherals::ADC1,
};

pub struct ChargeState {
    pub percent: i8,
    pub volts: u16,
}

const SAMPLE_COUNT: usize = 9;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1);

// DOCS: https://wiki.seeedstudio.com/reterminal_e10xx_with_arduino/#battery-management-system
// See https://github.com/GuySie/random-things/blob/main/Seeed-reTerminal-E1002/seeed-reterminal-art-display.yaml
// I STOLE THE CODE FROM HERE: https://github.com/Frans-Willem/epd-photoframe/blob/master/src/battery.rs?utm_source=copilot.com
// Seems to work okay but could be AI slop, i dunno yet

pub async fn get_charge_state(
    adc: ADC1<'static>,
    adc_channel: impl AdcChannel + AnalogPin,
    enable_pin: impl OutputPin,
) -> ChargeState {
    info!("[BAT] ADC config");
    let mut adc1_config = AdcConfig::new();
    let mut adc_pin = adc1_config.enable_pin(adc_channel, Attenuation::_11dB);
    let mut adc = Adc::new(adc, adc1_config);

    let mut battery_measure_enable_pin =
        Output::new(enable_pin, Level::High, OutputConfig::default());

    battery_measure_enable_pin.set_high();

    let mut samples = [0u16; SAMPLE_COUNT];
    for (i, sample) in samples.iter_mut().enumerate() {
        if i > 0 {
            Timer::after(SAMPLE_INTERVAL).await;
        }
        *sample = adc.read_blocking(&mut adc_pin);
    }
    battery_measure_enable_pin.set_low();

    samples.sort_unstable();

    // The curve calibration scheme returns mV directly; the ÷2 divider
    // on the board halves V_BAT into the ADC, so multiply by 2 to
    // recover battery voltage.
    let battery_mv = samples[SAMPLE_COUNT / 2].saturating_mul(2);

    // Apperently re-terminal divides the voltage by 2 because it slipts it beteween
    // So i fully charged the device and then fudged this number until the voltage read 4.2
    // I've seen people use x2 but also with 12db attenuation
    // let adc_value = adc_value * 2.0;

    // info!("[BAT] ADC value {}", adc_value);

    // Need to turn this off after we're done reading voltage
    battery_measure_enable_pin.set_low();

    // let volts = adc_to_v(adc_value);

    let percent = voltage_to_battery_percentage(battery_mv);

    info!("[BAT] {} (v{})", percent, battery_mv);

    return ChargeState { percent, volts: battery_mv };
}

fn voltage_to_battery_percentage(mv: u16) -> i8 {
    const CURVE: &[(u16, u8)] = &[
        (3270, 0),
        (3300, 5),
        (3410, 10),
        (3490, 20),
        (3580, 30),
        (3680, 40),
        (3750, 50),
        (3800, 60),
        (3850, 70),
        (3910, 80),
        (3960, 90),
        (4150, 100),
    ];
    if mv <= CURVE[0].0 {
        return 0;
    }
    if mv >= CURVE[CURVE.len() - 1].0 {
        return 100;
    }
    for window in CURVE.windows(2) {
        let (v0, p0) = window[0];
        let (v1, p1) = window[1];
        if mv <= v1 {
            let dx = (mv - v0) as u32;
            let span = (v1 - v0) as u32;
            let dy = (p1 - p0) as u32;
            return (p0 as u32 + dx * dy / span) as i8;
        }
    }
    0 // unreachable — clamped above
}
