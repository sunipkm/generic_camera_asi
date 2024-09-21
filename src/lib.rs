#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! # generic-camera-asi
//! Crate implementing the traits in the [`generic-camera`](https://crates.io/crates/generic-camera) crate for ZWO ASI cameras.
//!
//! # Usage
//! ```
//! use generic_camera::{GenCam, GenCamDriver};
//! use generic_camera_asi::{GenCamAsi, GenCamDriverAsi};
//! use std::{thread::sleep, time::Duration};
//!  
//! let mut drv = GenCamDriverAsi;
//! if drv.available_devices() == 0 {
//!     return;
//! }
//! let mut cam = drv.connect_first_device().expect("Could not connect to camera");
//! cam.start_exposure().expect("Could not start exposure");
//! while !cam.image_ready().expect("Could not check if image is ready") {
//!     sleep(Duration::from_secs(1));
//! }
//! let img = cam
//!           .download_image()
//!           .expect("Could not download image");
//! ```
mod asicamera2;
mod asihandle;
mod zwo_ffi;
#[macro_use]
mod zwo_ffi_wrapper;

pub use asicamera2::{GenCamAsi, GenCamDriverAsi};
pub use asihandle::GenCamInfoAsi;

pub use generic_camera::*;

// Can't use the macro-call itself within the `doc` attribute. So force it to eval it as part of
// the macro invocation.
//
// The inspiration for the macro and implementation is from
// <https://github.com/GuillaumeGomez/doc-comment>
//
// MIT License
//
// Copyright (c) 2018 Guillaume Gomez
macro_rules! insert_as_doc {
    { $content:expr } => {
        #[allow(unused_doc_comments)]
        #[doc = $content] extern { }
    }
}

// Provides the README.md as doc, to ensure the example works!
insert_as_doc!(include_str!("../README.MD"));
