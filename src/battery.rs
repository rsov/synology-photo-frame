use defmt::info;
use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcConfig, Attenuation},
    gpio::{AnalogPin, Level, Output, OutputConfig, OutputPin},
    peripherals::ADC1,
};

pub struct ChargeState {
    pub percent: i8,
    pub volts: f64,
}

// DOCS: https://wiki.seeedstudio.com/reterminal_e10xx_with_arduino/#battery-management-system
// See https://github.com/GuySie/random-things/blob/main/Seeed-reTerminal-E1002/seeed-reterminal-art-display.yaml

pub fn get_charge_state(
    adc: ADC1,
    adc_channel: impl AdcChannel + AnalogPin,
    enable_pin: impl OutputPin,
) -> ChargeState {
    info!("[BAT] ADC config");
    let mut adc1_config = AdcConfig::new();
    let mut adc_pin = adc1_config.enable_pin(adc_channel, Attenuation::_11dB);
    let mut adc = Adc::new(adc, adc1_config);

    let mut battery_measure_enable_pin =
        Output::new(enable_pin, Level::High, OutputConfig::default());

    let adc_value = adc.read_blocking(&mut adc_pin) as f64;
    // So i fully charged the device and then fudged this number until the voltage read 4.2
    // I've seen people use x2 but also with 12db attenuation
    let adc_value = adc_value * 2.65;

    info!("[BAT] ADC value {}", adc_value);

    // Need to turn this off after we're done reading voltage
    battery_measure_enable_pin.set_low();

    let volts = adc_to_v(adc_value);

    let percent = voltage_to_battery_percentage(volts);

    info!("[BAT] {} (v{})", percent, volts);

    return ChargeState { percent, volts };
}

// not sure why adc doesn't give mill-amps right away
fn adc_to_v(adc: f64) -> f64 {
    adc * 3.3 / 4095.0
}

fn voltage_to_battery_percentage(v: f64) -> i8 {
    let v_max = 4.20; // blaze it lol
    let v_min = 3.27;

    let pct = (v - v_min) / (v_max - v_min) * 100.0;

    return pct.clamp(0.0, 100.0) as i8;
}
