# cameraunit_asi

`cameraunit_asi` implements the API traits provided by [`cameraunit`]("https://github.com/sunipkm/cameraunit)
to capture frames from CCD/CMOS based detectors from [ZWO](https://www.zwoastro.com/). This crate provides
wrappers for the [ASI Camera SDK](https://www.zwoastro.com/downloads/developers) C library to access the
cameras for image capture and other housekeeping functions in a safe way. Images are obtained as 
`cameraunit::ImageData` with extensive metadata.

As is, this Rust driver is intended for use on Linux and macOS platforms.

You can use `cameraunit_asi` to:
 - Access a connected ZWO ASI camera,
 - Acquire images from the in supported pixel formats (using the [`image`](https://crates.io/crates/image) crate as a backend),
 - Save these images to `FITS` files (requires the `cfitsio` C library, and uses the [`fitsio`](https://crates.io/crates/fitsio) crate) with extensive metadata,
 - Alternatively, use the internal [`image::DynamicImage`](https://docs.rs/image/0.24.7/image/enum.DynamicImage.html) object to obtain `JPEG`, `PNG`, `BMP` etc.

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
cameraunit_asi = "4.1"
```
and this to your source code:
```rs
use cameraunit::{CameraUnit, CameraInfo, ImageData};
use cameraunit_asi::{num_cameras, open_first_camera, ASIImageFormat};
```

## Example
Minimally, the following can open the first available camera, capture a single image, and save it to a `FITS` file:
```rs
let nc = num_cameras();
if nc <= 0 {
    return;
}
let (mut cam, _caminfo) = open_first_camera()
    .map_err(|x| println!("Opening camera: {}", x.to_string()))
    .unwrap();
cam.set_exposure(Duration::from_millis(700))
    .map_err(|x| println!("Setting exposure: {}", x.to_string()))
    .unwrap();
cam.start_exposure()
    .map_err(|x| println!("Start exposure: {}", x.to_string()))
    .unwrap();
while !cam
    .image_ready()
    .map_err(|x| println!("Check exposure: {}", x.to_string()))
    .unwrap()
{
    sleep(Duration::from_secs(1));
}
let img = cam
    .download_image()
    .map_err(|x| println!("Downloading image: {}", x.to_string()))
    .unwrap();
img.save_fits(Path::new("./"), "test", "asicam_test", true, true)
    .unwrap();
```
Note, that the unused `_caminfo` object implements the `cameraunit::CameraInfo` trait and can be cloned and passed around
to multiple threads.

For a more complete example, refer to the [bundled program](src/main.rs).

## Installation
The example program can be installed using
```sh
$ cargo install cameraunit_asi
```
and executed using
```sh
$ asicamera_capture
```
