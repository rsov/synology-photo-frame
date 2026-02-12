# What is

ReTerminal E1002 color display picture frame to view photos from Synology Photos album


# Notes

See https://github.com/Frans-Willem/reterminal_e100x for stuff


Data https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004321_reTerminal_E1002_V1.0_SCH_250805.pdf


### Wrong bits per pixel set

Looks like the feature branch that has a screen has a bug with setting how many pixels a byte takes up

https://github.com/RitwikSaikia/epd-waveshare feat/epd7in3e_impl

Should be `RawU8` instead of `RawU4`
```rust
#[cfg(feature = "graphics")]
impl PixelColor for HexColor {
    type Raw = embedded_graphics_core::pixelcolor::raw::RawU8;
}
```

An image thats 600x338 will render at 600x676

Embedded graphics will take the bytes per pixels to figure out how long the image is based on buffer length

