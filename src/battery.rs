use defmt::info;
use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcConfig, Attenuation},
    gpio::{AnalogPin, Level, Output, OutputConfig, OutputPin},
    peripherals::ADC1,
};

// DOCS: https://wiki.seeedstudio.com/reterminal_e10xx_with_arduino/#battery-management-system
// See https://github.com/GuySie/random-things/blob/main/Seeed-reTerminal-E1002/seeed-reterminal-art-display.yaml

pub fn get_battery_percent(
    adc: ADC1,
    adc_channel: impl AdcChannel + AnalogPin,
    enable_pin: impl OutputPin,
) -> i8 {
    info!("[BAT] ADC config");
    let mut adc1_config = AdcConfig::new();
    let mut adc_pin = adc1_config.enable_pin(adc_channel, Attenuation::_11dB);
    let mut adc = Adc::new(adc, adc1_config);

    let mut battery_measure_enable_pin =
        Output::new(enable_pin, Level::High, OutputConfig::default());

    let adc_value = adc.read_blocking(&mut adc_pin);

    info!("[BAT] value {}", adc_value);

    // Need to turn this off after we're done reading voltage
    battery_measure_enable_pin.set_low();

    let voltage_raw = adc_to_v(adc_value);

    info!("[BAT] RAW v{}", voltage_raw);

    return voltage_to_battery_percentage(voltage_raw);
}

// not sure why adc doesn't give mill-amps right away
fn adc_to_v(adc: u16) -> f64 {
    (adc as f64 * 3.3) / 4095.0
}

fn voltage_to_battery_percentage(v: f64) -> i8 {
    // I've seen it go down to 1.5 when fully drained so this is very conservative
    let v_max = 4.2;
    let v_min = 3.00;

    let pct = (v - v_min) / (v_max - v_min) * 100.0;

    return pct.clamp(0.0, 100.0) as i8;
}
