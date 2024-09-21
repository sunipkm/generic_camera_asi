#![warn(missing_docs)]
use std::{collections::HashMap, time::Duration};

use generic_camera::{
    AnyGenCamInfo, GenCam, GenCamCtrl, GenCamDescriptor, GenCamDriver, GenCamError, GenCamResult,
    GenericImage, Property, PropertyValue,
};

use crate::{
    asihandle::{get_asi_devs, open_device, AsiImager},
    zwo_ffi::ASIGetNumOfConnectedCameras,
    zwo_ffi_wrapper::AsiError,
};

#[derive(Debug, Default)]
/// [`GenCamDriver`] implementation for ASI cameras.
///
/// This struct is used to list and connect to ASI cameras.
///
/// # Examples
/// ```
/// use generic_camera::GenCamDriver;
/// use generic_camera_asi::GenCamDriverAsi;
///
/// let mut driver = GenCamDriverAsi::default();
/// let num_devices = driver.available_devices();
/// if num_devices > 0 {
///     let devices = driver.list_devices();
///     let first_device = driver.connect_first_device();
/// }
/// ```
pub struct GenCamDriverAsi;

impl GenCamDriver for GenCamDriverAsi {
    fn available_devices(&self) -> usize {
        let res = unsafe { ASIGetNumOfConnectedCameras() };
        res as usize
    }

    fn list_devices(&mut self) -> GenCamResult<Vec<generic_camera::GenCamDescriptor>> {
        get_asi_devs().map_err(|e| match e {
            AsiError::InvalidId(_, _) => GenCamError::InvalidIndex(0),
            AsiError::CameraRemoved(_, _) => GenCamError::CameraRemoved,
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })
    }

    fn connect_device(
        &mut self,
        descriptor: &generic_camera::GenCamDescriptor,
    ) -> GenCamResult<generic_camera::AnyGenCam> {
        let handle = open_device(descriptor)?;
        let caps = handle.get_concat_caps();
        Ok(Box::new(GenCamAsi { handle, caps }))
    }

    fn connect_first_device(&mut self) -> GenCamResult<generic_camera::AnyGenCam> {
        let devs = self.list_devices()?;
        if devs.is_empty() {
            return Err(GenCamError::NoCamerasAvailable);
        }
        self.connect_device(&devs[0])
    }
}

/// Generic camera control for ASI cameras.
///
/// Implements the [`GenCam`] trait for ASI cameras.
///
/// # Examples
/// ```
/// use generic_camera::{GenCam, GenCamDriver};
/// use generic_camera_asi::{GenCamAsi, GenCamDriverAsi};
///
/// let mut drv = GenCamDriverAsi::default();
/// if let Ok(mut cam) = drv.connect_first_device() {
///     println!("Connected to camera: {}", cam.camera_name());
/// } else {
///     println!("No cameras available");
/// }
///

#[derive(Debug)]
pub struct GenCamAsi {
    handle: AsiImager,
    caps: HashMap<GenCamCtrl, Property>,
}

impl GenCam for GenCamAsi {
    fn start_exposure(&mut self) -> GenCamResult<()> {
        self.handle.start_exposure()
    }

    fn info(&self) -> GenCamResult<&GenCamDescriptor> {
        Ok(self.handle.get_descriptor())
    }

    fn image_ready(&self) -> GenCamResult<bool> {
        self.handle.image_ready()
    }

    fn download_image(&mut self) -> GenCamResult<GenericImage> {
        self.handle.download_image()
    }

    fn info_handle(&self) -> Option<AnyGenCamInfo> {
        Some(Box::new(self.handle.get_info_handle()))
    }

    fn vendor(&self) -> &str {
        "ZWO"
    }

    fn camera_ready(&self) -> bool {
        true
    }

    fn camera_name(&self) -> &str {
        self.handle.camera_name()
    }

    fn list_properties(&self) -> &std::collections::HashMap<GenCamCtrl, generic_camera::Property> {
        &self.caps
    }

    fn set_property(
        &mut self,
        name: GenCamCtrl,
        value: &generic_camera::PropertyValue,
        auto: bool,
    ) -> GenCamResult<()> {
        self.handle.set_property(&name, value, auto)
    }

    fn cancel_capture(&self) -> GenCamResult<()> {
        self.handle.stop_exposure()
    }

    fn is_capturing(&self) -> bool {
        self.handle.is_capturing()
    }

    fn capture(&mut self) -> GenCamResult<GenericImage> {
        let (exp, _) = self.handle.get_exposure()?;
        self.handle.start_exposure()?;
        std::thread::sleep(exp);
        while !self.handle.image_ready()? {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.handle.download_image()
    }

    fn camera_state(&self) -> GenCamResult<generic_camera::GenCamState> {
        self.handle.get_state()
    }

    fn set_roi(
        &mut self,
        roi: &generic_camera::GenCamRoi,
    ) -> GenCamResult<&generic_camera::GenCamRoi> {
        self.handle.set_roi(roi)
    }

    fn get_roi(&self) -> &generic_camera::GenCamRoi {
        self.handle.get_roi()
    }

    fn get_property(&self, name: GenCamCtrl) -> GenCamResult<(PropertyValue, bool)> {
        self.handle.get_property(&name)
    }
}
