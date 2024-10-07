# generic-camera-asi

`generic-camera-asi` implements the API traits provided by [`generic-camera`]("https://crates.io/crates/generic-camera")
to capture frames from CCD/CMOS based detectors from [ZWO](https://www.zwoastro.com/). This crate provides
wrappers for the [ASI Camera SDK](https://www.zwoastro.com/downloads/developers) C library to access the
cameras for image capture and other housekeeping functions in a safe way. Images are obtained as 
[`refimage::GenericImage`](https://docs.rs/refimage/latest/refimage/struct.GenericImage.html) with extensive metadata.

As is, this Rust driver is intended for use on Linux and macOS platforms.

You can use `generic-camera-asi` to:
 - Access a connected ZWO ASI camera,
 - Acquire images from the in supported pixel formats (using the [`image`](https://crates.io/crates/image) crate as a backend),
 - Save these images to `FITS` files (requires the `cfitsio` C library, and uses the [`fitsio`](https://crates.io/crates/fitsio) crate) with extensive metadata,
 - Alternatively, use the internal [`image::DynamicImage`](https://docs.rs/image/latest/image/enum.DynamicImage.html) object to obtain `JPEG`, `PNG`, `BMP` etc.

## Pre-requisite
 1. Install `libusb-1.0-dev` on your system.
 1. Obtain the [ZWO ASI Camera SDK](https://www.zwoastro.com/downloads/developers).
 1. Extract the `ASI_linux_mac_SDK_VX.XX.tar.bz2` from the ZIP, and extract its contents (`tar -xf ASI_linux_mac_SDK_VX.XX.tar.bz2`), which will extract the contents to `ASI_linux_mac_SDK_VX.XX` in the current directory.
 1. Copy `ASI_linux_mac_SDK_VX.XX/include/ASICamera2.h` to `/usr/local/include`, or any other directory in your include path.
 1. Open `README.txt` in `ASI_linux_mac_SDK_VX.XX/lib` to determine the applicable system platform. Follow the additional commands to install the `udev` rules so that the cameras can be accessed without `sudo`.
 1. Copy `ASI_linux_mac_SDK_VX.XX/lib/your_target_platform/libASICamera*` to a directory in your library path (probably `/usr/local/lib`), and ensure `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS) contains the library path.

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
generic-camera-asi = "<1.0"
```
and this to your source code:
```rs
use generic_camera::{GenCam, GenCamDriver};
use generic_camera_asi::{GenCamAsi, GenCamDriverAsi};
use std::{thread::sleep, time::Duration};
```

## Example
Minimally, the following can open the first available camera and capture a single image:
```rs
let mut drv = GenCamDriverAsi;
if drv.available_devices() == 0 {
    return;
}
let mut cam = drv.connect_first_device().expect("Could not connect to camera");
cam.start_exposure().expect("Could not start exposure");
while !cam.image_ready().expect("Could not check if image is ready") {
    sleep(Duration::from_secs(1));
}
let img = cam
          .download_image()
          .expect("Could not download image");
```

For a more complete example, refer to the [bundled program](gencam_asi_example/src/main.rs).
## Features
Activate the `bayerswap` feature to swap the Bayer mosaic conversion.