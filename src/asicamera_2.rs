#![warn(missing_docs)]

use std::{
    collections::HashMap,
    ffi::{c_long, c_uchar, CStr},
    fmt::Display,
    mem::MaybeUninit,
    os::raw,
    str,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::zwo_ffi::*;

use cameraunit::{
    CameraInfo, CameraUnit, DynamicSerialImage, Error, ImageMetaData, PixelBpp, SerialImageBuffer,
    ROI,
};
use log::warn;

/// This object describes a ZWO ASI camera, and provides methods for control and image capture.
///
/// This object implements the `CameraUnit` and `CameraInfo` trait.
pub struct CameraUnitASI {
    id: Arc<ASICamId>,
    capturing: Arc<Mutex<bool>>,
    props: Box<ASICameraProps>,
    cooler_on: Arc<AtomicBool>,
    // control_caps: Vec<ASIControlCaps>,
    gain_min: i64,
    gain_max: i64,
    exp_min: Duration,
    exp_max: Duration,
    exposure: Duration,
    is_dark_frame: bool,
    image_fmt: ASIImageFormat,
    roi: ROI,
    last_img_start: Mutex<SystemTime>,
}

#[derive(Clone)]
/// This object describes a ZWO ASI camera and provides methods for obtaining housekeeping data.
///
/// This object implements the [`cameraunit::CameraInfo`] trait, and additionally the [`std::clone::Clone`] trait.
pub struct CameraInfoASI {
    id: Arc<ASICamId>,
    capturing: Arc<Mutex<bool>>,
    cooler_on: Arc<AtomicBool>,
    name: String,
    uuid: [u8; 8],
    height: u32,
    width: u32,
    psize: f64,
    is_cooler_cam: bool,
}

#[derive(Clone)]
/// This object describes the properties of the ZWO ASI camera.
///
/// This object implements the [`std::fmt::Display`] and [`std::clone::Clone`] traits.
pub struct ASICameraProps {
    name: String,
    id: i32,
    uuid: [u8; 8],
    max_height: u32,
    max_width: u32,
    is_color_cam: bool,
    bayer_pattern: Option<ASIBayerPattern>,
    supported_bins: Vec<u32>,
    supported_formats: Vec<ASIImageFormat>,
    pixel_size: f64,
    mechanical_shutter: bool,
    is_cooler_cam: bool,
    is_usb3_camera: bool,
    e_per_adu: f32,
    bit_depth: i32,
    is_trigger_camera: bool,
}

/// Get the number of available ZWO ASI cameras.
///
/// # Examples
///
/// ```
/// let num_cameras = cameraunit_asi::num_cameras();
/// if num_cameras <= 0 {
///     println!("No cameras found");
/// }
/// // proceed to get camera IDs and information
/// ```
pub fn num_cameras() -> i32 {
    unsafe { ASIGetNumOfConnectedCameras() }
}

/// Get the IDs and names of the available ZWO ASI cameras.
///
/// # Examples
///
/// ```
/// let cam_ids = cameraunit_asi::get_camera_ids();
/// if let Some(cam_ids) = cam_ids {
///     // do stuff with camera IDs and names
/// }
/// ```
pub fn get_camera_ids() -> Option<HashMap<i32, String>> {
    let num_cameras = num_cameras();
    if num_cameras > 0 {
        let mut map: HashMap<i32, String> = HashMap::with_capacity(num_cameras as usize);
        for i in 0..num_cameras {
            let info = get_camera_prop_by_idx(i);
            if info.is_err() {
                continue;
            } else {
                let info = info.unwrap();
                map.insert(info.CameraID, string_from_char(&info.Name));
            }
        }
        if map.is_empty() {
            return None;
        }
        Some(map)
    } else {
        None
    }
}

/// Open a ZWO ASI camera by ID for access.
///
/// This method, if successful, returns a tuple containing a `CameraUnit_ASI` object and a `CameraInfo_ASI` object.
/// The `CameraUnit_ASI` object allows for control of the camera and image capture, while the `CameraInfo_ASI` object
/// only allows for access to housekeeping data.
///
/// The `CameraUnit_ASI` object is required for image capture, and should
/// be mutable in order to set exposure, ROI, gain, etc.
///
/// The `CameraInfo_ASI` object allows cloning and sharing, and is useful for obtaining housekeeping data from separate
/// threads.
///
/// # Arguments
///
/// * `id` - The ID of the camera to open. This ID can be obtained from the `get_camera_ids()` method.
///
/// # Errors
///  - [`cameraunit::Error::InvalidId`] - The ID provided is not valid.
///  - [`cameraunit::Error::CameraClosed`] - The camera is closed.
///  - [`cameraunit::Error::NoCamerasAvailable`] - No cameras are available.
///
/// # Examples
///
/// ```no_run
/// use cameraunit_asi::open_camera;
/// let id: i32 = 0; // some ID obtained using get_camera_ids()
/// if let Ok((mut cam, caminfo)) = open_camera(id) {
///
/// }
/// // do things with cam
/// ```
pub fn open_camera(id: i32) -> Result<(CameraUnitASI, CameraInfoASI), Error> {
    if let Some(cam_ids) = get_camera_ids() {
        if !cam_ids.contains_key(&id) {
            return Err(Error::InvalidId(id));
        }
        let info = get_camera_prop_by_id(id)?;

        let mut prop = ASICameraProps {
            name: string_from_char(&info.Name),
            id: info.CameraID,
            uuid: [0; 8],
            max_height: info.MaxHeight as u32,
            max_width: info.MaxWidth as u32,
            is_color_cam: info.IsColorCam == ASI_BOOL_ASI_TRUE,
            bayer_pattern: if info.IsColorCam == ASI_BOOL_ASI_TRUE {
                ASIBayerPattern::from_u32(info.BayerPattern)
            } else {
                None
            },
            supported_bins: {
                let mut bins: Vec<u32> = Vec::new();
                for x in info.SupportedBins.iter() {
                    if *x != 0 {
                        bins.push(*x as u32);
                    } else {
                        break;
                    }
                }
                bins
            },
            supported_formats: {
                let mut formats: Vec<ASIImageFormat> = Vec::new();
                for x in info.SupportedVideoFormat.iter() {
                    if *x >= 0 {
                        formats.push(ASIImageFormat::from_u32(*x as u32).unwrap());
                    } else {
                        break;
                    }
                }
                formats
            },
            pixel_size: info.PixelSize,
            mechanical_shutter: info.MechanicalShutter == ASI_BOOL_ASI_TRUE,
            is_cooler_cam: info.IsCoolerCam == ASI_BOOL_ASI_TRUE,
            is_usb3_camera: info.IsUSB3Host == ASI_BOOL_ASI_TRUE,
            e_per_adu: info.ElecPerADU,
            bit_depth: info.BitDepth,
            is_trigger_camera: info.IsTriggerCam == ASI_BOOL_ASI_TRUE,
        };

        if prop.is_usb3_camera {
            let cid = MaybeUninit::<ASI_ID>::zeroed();
            let mut cid = unsafe { cid.assume_init() };
            let res = unsafe { ASIGetID(id, &mut cid) };
            if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                return Err(Error::InvalidId(id));
            } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                return Err(Error::CameraClosed);
            }
            prop.uuid = cid.id;
        }

        let res = unsafe { ASIInitCamera(prop.id) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(prop.id));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }

        let ccaps = get_control_caps(prop.id)?;

        let (gain_min, gain_max) = get_gain_minmax(&ccaps);
        let (exp_min, exp_max) = get_exposure_minmax(&ccaps);

        let cobj = CameraUnitASI {
            id: Arc::new(ASICamId(prop.id)),
            capturing: Arc::new(Mutex::new(false)),
            props: Box::new(prop.clone()),
            cooler_on: Arc::new(AtomicBool::new(false)),
            // control_caps: ccaps,
            gain_min,
            gain_max,
            exp_min,
            exp_max,
            exposure: Duration::from_millis(100),
            is_dark_frame: false,
            image_fmt: {
                if prop.is_color_cam {
                    ASIImageFormat::ImageRGB24
                } else if prop.supported_formats.contains(&ASIImageFormat::ImageRAW16) {
                    ASIImageFormat::ImageRAW16
                } else {
                    ASIImageFormat::ImageRAW8
                }
            },
            roi: ROI {
                x_min: 0,
                y_min: 0,
                width: prop.max_width,
                height: prop.max_height,
                bin_x: 1,
                bin_y: 1,
            },
            last_img_start: Mutex::new(UNIX_EPOCH),
        };

        cobj.set_start_pos(0, 0)?;
        cobj.set_roi_format(&ASIRoiMode {
            width: cobj.roi.width as i32,
            height: cobj.roi.height as i32,
            bin: cobj.roi.bin_x as i32,
            fmt: cobj.image_fmt,
        })?;

        let cinfo = CameraInfoASI {
            id: cobj.id.clone(),
            capturing: cobj.capturing.clone(),
            cooler_on: cobj.cooler_on.clone(),
            name: prop.name.clone(),
            uuid: prop.uuid,
            height: prop.max_height as u32,
            width: prop.max_width as u32,
            psize: prop.pixel_size,
            is_cooler_cam: prop.is_cooler_cam,
        };

        Ok((cobj, cinfo))
    } else {
        Err(Error::NoCamerasAvailable)
    }
}

/// Open the first available ZWO ASI camera for access.
///
/// This method, if successful, returns a tuple containing a `CameraUnit_ASI` object and a `CameraInfo_ASI` object.
/// The `CameraUnit_ASI` object allows for control of the camera and image capture, while the `CameraInfo_ASI` object
/// only allows for access to housekeeping data.
///
/// The `CameraUnit_ASI` object is required for image capture, and should
/// be mutable in order to set exposure, ROI, gain, etc.
///
/// The `CameraInfo_ASI` object allows cloning and sharing, and is useful for obtaining housekeeping data from separate
/// threads.
///
/// # Errors
///  - [`cameraunit::Error::InvalidId`] - The ID provided is not valid.
///  - [`cameraunit::Error::CameraClosed`] - The camera is closed.
///  - [`cameraunit::Error::NoCamerasAvailable`] - No cameras are available.
///
/// # Examples
///
/// ```no_run
/// use cameraunit_asi::open_first_camera;
///
/// if let Ok((mut cam, caminfo)) = open_first_camera() {
///
/// }
/// ```
pub fn open_first_camera() -> Result<(CameraUnitASI, CameraInfoASI), Error> {
    let ids = get_camera_ids();
    if let Some(ids) = ids {
        let val = ids.iter().next().unwrap();
        open_camera(*val.0)
    } else {
        Err(Error::NoCamerasAvailable)
    }
}

#[deny(missing_docs)]
impl CameraUnitASI {
    /// Set an unique identifier for the camera.
    ///
    /// This method is only available for USB3 cameras.
    ///
    /// # Arguments
    ///  * `uuid` - The unique identifier to set. This must be an array of 8 unsigned bytes.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidMode`] - The camera does not support setting a UUID.
    ///  - [`cameraunit::Error::InvalidId`] - The camera ID is invalid.
    ///  - [`cameraunit::Error::CameraClosed`] - The camera is closed.
    pub fn set_uuid(&mut self, uuid: &[u8; 8]) -> Result<(), Error> {
        if !self.props.is_usb3_camera {
            return Err(Error::InvalidMode(
                "Camera does not support UUID".to_owned(),
            ));
        }
        if str::from_utf8(uuid).is_err() {
            return Err(Error::InvalidValue(
                "UUID must be a valid UTF-8 string".to_owned(),
            ));
        }
        if self.props.uuid == *uuid {
            Ok(())
        } else {
            let cid = ASI_ID { id: *uuid };
            let res = unsafe { ASISetID(self.id.0, cid) };
            if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                return Err(Error::InvalidId(self.id.0));
            } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                return Err(Error::CameraClosed);
            }
            self.props.uuid = *uuid;
            Ok(())
        }
    }

    /// Get the backend SDK version.
    pub fn get_sdk_version() -> String {
        let c_buf = unsafe { ASIGetSDKVersion() };
        let c_str: &CStr = unsafe { CStr::from_ptr(c_buf) };
        let str_slice: &str = c_str.to_str().unwrap();
        str_slice.to_owned()
    }

    /// Get the camera serial number.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - The camera ID is invalid.
    ///  - [`cameraunit::Error::GeneralError`] - The camera does not have a serial number.
    pub fn get_serial(&self) -> Result<u64, Error> {
        let ser = MaybeUninit::<ASI_SN>::zeroed();
        let mut ser = unsafe { ser.assume_init() };
        let res = unsafe { ASIGetSerialNumber(self.id.0, &mut ser) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR as i32 {
            return Err(Error::GeneralError(
                "Camera does not have serial number.".to_owned(),
            ));
        }
        let ser = u64::from_be_bytes(ser.id);
        Ok(ser)
    }

    /// Get the camera image format.
    pub fn get_image_fmt(&self) -> ASIImageFormat {
        self.image_fmt
    }

    /// Set the camera image format.
    ///
    /// # Arguments
    ///  * `fmt` - The image format to set. The format is a pixel format from the `ASIImageFormat` enum.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidMode`] - The camera does not support the specified image format.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is in progress.
    ///  - [`cameraunit::Error::InvalidId`] - The camera ID is invalid.
    ///  - [`cameraunit::Error::CameraClosed`] - The camera is closed.
    ///  - [`cameraunit::Error::InvalidMode`] - The camera does not support the specified image format.
    pub fn set_image_fmt(&mut self, fmt: ASIImageFormat) -> Result<(), Error> {
        if self.image_fmt == fmt {
            return Ok(());
        }
        if !self.props.supported_formats.contains(&fmt) {
            return Err(Error::InvalidMode(format!(
                "Format {:?} not supported by camera",
                fmt
            )));
        }
        if self.is_capturing() {
            return Err(Error::ExposureInProgress);
        }
        let mut roi = self.get_roi_format()?;
        roi.fmt = fmt;
        self.set_roi_format(&roi)?;
        self.image_fmt = fmt;
        Ok(())
    }

    /// Get the camera properties.
    pub fn get_props(&self) -> &ASICameraProps {
        &self.props
    }

    /// Get the internal ROI format.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - The camera ID is invalid.
    ///  - [`cameraunit::Error::CameraClosed`] - The camera is closed.
    ///  - [`cameraunit::Error::InvalidMode`] - The camera does not support the specified image format.
    fn get_roi_format(&self) -> Result<ASIRoiMode, Error> {
        let mut roi = ASIRoiMode {
            width: 0,
            height: 0,
            bin: 0,
            fmt: ASIImageFormat::ImageRAW8,
        };
        let mut fmt: i32 = 0;
        let res = unsafe {
            ASIGetROIFormat(
                self.id.0,
                &mut roi.width,
                &mut roi.height,
                &mut roi.bin,
                &mut fmt,
            )
        };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }
        if let Some(fmt) = ASIImageFormat::from_u32(fmt as u32) {
            roi.fmt = fmt;
            Ok(roi)
        } else {
            Err(Error::InvalidMode(format!("Invalid image format: {}", fmt)))
        }
    }

    fn set_roi_format(&self, roi: &ASIRoiMode) -> Result<(), Error> {
        let res =
            unsafe { ASISetROIFormat(self.id.0, roi.width, roi.height, roi.bin, roi.fmt as i32) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }
        Ok(())
    }

    fn set_start_pos(&self, x: i32, y: i32) -> Result<(), Error> {
        if x < 0 || y < 0 {
            return Err(Error::InvalidValue(format!(
                "Invalid start position: {}, {}",
                x, y
            )));
        }
        let res = unsafe { ASISetStartPos(self.id.0, x, y) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        } else if res == ASI_ERROR_CODE_ASI_ERROR_OUTOF_BOUNDARY as i32 {
            return Err(Error::OutOfBounds(format!(
                "Could not set start position to {}, {}",
                x, y
            )));
        }
        Ok(())
    }

    fn get_start_pos(&self) -> Result<(i32, i32), Error> {
        let mut x: i32 = 0;
        let mut y: i32 = 0;
        let res = unsafe { ASIGetStartPos(self.id.0, &mut x, &mut y) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }
        Ok((x, y))
    }

    fn get_exposure_status(&self) -> Result<ASIExposureStatus, Error> {
        let stat = MaybeUninit::<ASI_EXPOSURE_STATUS>::zeroed();
        let mut stat = unsafe { stat.assume_init() };
        let res = unsafe { ASIGetExpStatus(self.id.0, &mut stat) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(self.id.0));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }
        ASIExposureStatus::from_u32(stat)
    }
}

#[deny(missing_docs)]
impl CameraInfo for CameraInfoASI {
    /// Cancel an exposure in progress.
    /// This function may panic if the internal mutex is poisoned.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed
    fn cancel_capture(&self) -> Result<(), Error> {
        let mut capturing = self.capturing.lock().unwrap();
        if !*capturing {
            return Ok(());
        }
        sys_cancel_capture(self.id.0)?;
        *capturing = false;
        Ok(())
    }

    /// Get the camera name.
    fn camera_name(&self) -> &str {
        &self.name
    }

    fn get_uuid(&self) -> Option<&str> {
        str::from_utf8(&self.uuid).ok()
    }

    fn get_ccd_height(&self) -> u32 {
        self.height
    }

    fn get_ccd_width(&self) -> u32 {
        self.width
    }

    fn get_pixel_size(&self) -> Option<f32> {
        Some(self.psize as f32)
    }

    /// For ZWO ASI cameras, this always returns true.
    fn camera_ready(&self) -> bool {
        true
    }

    /// Check if the camera is capturing an image.
    /// This function may panic if the internal mutex is poisoned.
    fn is_capturing(&self) -> bool {
        let res = self.capturing.try_lock();
        match res {
            Ok(capturing) => *capturing,
            Err(_) => true,
        }
    }

    fn get_cooler_power(&self) -> Option<f32> {
        get_cooler_power(self.id.0)
    }

    fn get_temperature(&self) -> Option<f32> {
        get_temperature(self.id.0)
    }

    /// Turn the cooler on or off.
    ///
    /// # Arguments
    ///  * `on` - `true` to turn the cooler on, `false` for off.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a cooler
    ///  - [`cameraunit::Error::InvalidValue`] - Invalid control value
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    fn set_cooler(&self, on: bool) -> Result<(), Error> {
        set_control_value(
            self.id.0,
            ASIControlType::CoolerOn,
            if on { 1 } else { 0 },
            false,
        )?;
        self.cooler_on.store(on, Ordering::SeqCst);
        Ok(())
    }

    /// Check if the cooler is on or off.
    ///
    /// Returns `Some(true)` if cooler is on, else `Some(false)`.
    fn get_cooler(&self) -> Option<bool> {
        Some(self.cooler_on.load(Ordering::SeqCst))
    }

    /// Set the camera temperature.
    ///
    /// # Arguments
    ///  * `temperature` - Target temperature in degrees Celsius, must be between -80 C and 20 C.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a cooler
    ///  - [`cameraunit::Error::InvalidValue`] - Temperature is outside of range
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    fn set_temperature(&self, temperature: f32) -> Result<f32, Error> {
        let temp = set_temperature(self.id.0, temperature, self.is_cooler_cam)?;
        self.cooler_on.store(true, Ordering::SeqCst);
        Ok(temp)
    }
}

impl CameraInfo for CameraUnitASI {
    /// Cancel an exposure in progress.
    /// This function may panic if the internal mutex is poisoned.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed
    fn cancel_capture(&self) -> Result<(), Error> {
        let mut capturing = self.capturing.lock().unwrap();
        if !*capturing {
            return Ok(());
        }
        sys_cancel_capture(self.id.0)?;
        *capturing = false;
        Ok(())
    }

    /// Get the camera name.
    fn camera_name(&self) -> &str {
        &self.props.name
    }

    fn get_uuid(&self) -> Option<&str> {
        str::from_utf8(&self.props.uuid).ok()
    }

    fn get_ccd_height(&self) -> u32 {
        self.props.max_height
    }

    fn get_ccd_width(&self) -> u32 {
        self.props.max_width
    }

    fn get_pixel_size(&self) -> Option<f32> {
        Some(self.props.pixel_size as f32)
    }

    /// For ZWO ASI cameras, this always returns true.
    fn camera_ready(&self) -> bool {
        true
    }

    /// Check if the camera is capturing an image.
    /// This function may panic if the internal mutex is poisoned.
    fn is_capturing(&self) -> bool {
        let res = self.capturing.try_lock();
        match res {
            Ok(capturing) => *capturing,
            Err(_) => true,
        }
    }

    /// Turn the cooler on or off.
    ///
    /// # Arguments
    ///  * `on` - `true` to turn the cooler on, `false` for off.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a cooler
    ///  - [`cameraunit::Error::InvalidValue`] - Invalid control value
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    fn set_cooler(&self, on: bool) -> Result<(), Error> {
        set_control_value(
            self.id.0,
            ASIControlType::CoolerOn,
            if on { 1 } else { 0 },
            false,
        )?;
        self.cooler_on.store(on, Ordering::SeqCst);
        Ok(())
    }

    /// Check if the cooler is on or off.
    ///
    /// Returns `Some(true)` if cooler is on, else `Some(false)`.
    fn get_cooler(&self) -> Option<bool> {
        Some(self.cooler_on.load(Ordering::SeqCst))
    }

    fn get_cooler_power(&self) -> Option<f32> {
        get_cooler_power(self.id.0)
    }

    fn get_temperature(&self) -> Option<f32> {
        get_temperature(self.id.0)
    }

    /// Set the camera temperature.
    ///
    /// # Arguments
    ///  * `temperature` - Target temperature in degrees Celsius, must be between -80 C and 20 C.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a cooler
    ///  - [`cameraunit::Error::InvalidValue`] - Temperature is outside of range
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
    fn set_temperature(&self, temperature: f32) -> Result<f32, Error> {
        let temp = set_temperature(self.id.0, temperature, self.props.is_cooler_cam)?;
        self.cooler_on.store(true, Ordering::SeqCst);
        Ok(temp)
    }
}

impl CameraUnit for CameraUnitASI {
    /// Get the camera vendor. In this case, returns `ZWO`.
    fn get_vendor(&self) -> &str {
        "ZWO"
    }

    /// Get a handle to the internal camera. This is intended to be used for development purposes,
    /// as (presumably FFI and unsafe) internal calls are abstracted away from the user.
    ///
    /// The internal handle is of type `i32`.
    fn get_handle(&self) -> Option<&dyn std::any::Any> {
        Some(&self.id.0)
    }

    fn get_min_exposure(&self) -> Result<Duration, Error> {
        Ok(self.exp_min)
    }

    fn get_max_exposure(&self) -> Result<Duration, Error> {
        Ok(self.exp_max)
    }

    fn get_min_gain(&self) -> Result<i64, Error> {
        Ok(self.gain_min)
    }

    fn get_max_gain(&self) -> Result<i64, Error> {
        Ok(self.gain_max)
    }

    /// Capture an image.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::ExposureFailed`] - Exposure failed.
    ///  - [`cameraunit::Error::TimedOut`] - Exposure timed out.
    ///  - [`cameraunit::Error::GeneralError`] - The camera is in video capture mode.
    fn capture_image(&self) -> Result<DynamicSerialImage, Error> {
        let start_time: SystemTime;
        let roi: ASIRoiMode;
        {
            let mut capturing = self.capturing.lock().unwrap();
            let stat = self.get_exposure_status()?;
            if stat == ASIExposureStatus::Working {
                *capturing = true;
                return Err(Error::ExposureInProgress);
            } else if stat == ASIExposureStatus::Failed {
                *capturing = false;
                warn!("Exposure failed, retrying");
            } else if stat == ASIExposureStatus::Success {
                *capturing = false;
                warn!("Data from previous exposure not downloaded");
            }
            *capturing = false;
            roi = self.get_roi_format()?;
            *capturing = true;
            start_time = SystemTime::now();
            let res = unsafe {
                ASIStartExposure(
                    self.id.0,
                    if self.is_dark_frame {
                        ASI_BOOL_ASI_TRUE as i32
                    } else {
                        ASI_BOOL_ASI_FALSE as i32
                    },
                )
            };
            if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                *capturing = false;
                return Err(Error::InvalidId(self.id.0));
            } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                *capturing = false;
                return Err(Error::CameraClosed);
            } else if res == ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE as i32 {
                *capturing = true;
                return Err(Error::GeneralError("Video mode active".to_owned()));
            }
        }
        let mut stat: ASIExposureStatus;
        if self.exposure < Duration::from_millis(16) {
            loop {
                stat = self.get_exposure_status()?;
                if stat != ASIExposureStatus::Working {
                    break;
                }
                sleep(Duration::from_millis(1));
            }
        } else if self.exposure < Duration::from_secs(1) {
            loop {
                stat = self.get_exposure_status()?;
                if stat != ASIExposureStatus::Working {
                    break;
                }
                sleep(Duration::from_millis(100));
            }
        } else {
            loop {
                stat = self.get_exposure_status()?;
                if stat != ASIExposureStatus::Working {
                    break;
                }
                sleep(Duration::from_secs(1));
            }
        }
        let mut capturing = self.capturing.lock().unwrap(); // we are not dropping this until we return, so no problem reading exposure or roi
        if stat == ASIExposureStatus::Failed {
            *capturing = false;
            Err(Error::ExposureFailed("Unknown".to_owned()))
        } else if stat == ASIExposureStatus::Idle {
            *capturing = false;
            return Err(Error::ExposureFailed(
                "Successful exposure but no available data".to_owned(),
            ));
        } else if stat == ASIExposureStatus::Working {
            sys_cancel_capture(self.id.0)?;
            *capturing = false;
            return Err(Error::ExposureFailed("Exposure timed out".to_owned()));
        } else {
            let mut img = match roi.fmt {
                ASIImageFormat::ImageRAW8 => {
                    let mut data = vec![0u8; (roi.width * roi.height) as usize];
                    let res = unsafe {
                        ASIGetDataAfterExp(
                            self.id.0,
                            data.as_mut_ptr() as *mut c_uchar,
                            (roi.width * roi.height) as c_long,
                        )
                    };
                    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                        return Err(Error::InvalidId(self.id.0));
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                        return Err(Error::CameraClosed);
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                        return Err(Error::TimedOut);
                    }
                    *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                    let img: DynamicSerialImage = SerialImageBuffer::<u8>::from_vec(
                        roi.width as usize,
                        roi.height as usize,
                        data,
                    )
                    .unwrap()
                    .into();
                    img
                }
                ASIImageFormat::ImageRAW16 => {
                    let mut data = vec![0u16; (roi.width * roi.height) as usize];
                    let res = unsafe {
                        ASIGetDataAfterExp(
                            self.id.0,
                            data.as_mut_ptr() as *mut c_uchar,
                            (roi.width * roi.height * 2) as c_long,
                        )
                    };
                    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                        return Err(Error::InvalidId(self.id.0));
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                        return Err(Error::CameraClosed);
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                        return Err(Error::TimedOut);
                    }
                    *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                    let img: DynamicSerialImage = SerialImageBuffer::<u16>::from_vec(
                        roi.width as usize,
                        roi.height as usize,
                        data,
                    )
                    .unwrap()
                    .into();
                    img
                }
                ASIImageFormat::ImageRGB24 => {
                    let mut data = vec![0u8; (roi.width * roi.height * 3) as usize];
                    let res = unsafe {
                        ASIGetDataAfterExp(
                            self.id.0,
                            data.as_mut_ptr() as *mut c_uchar,
                            (roi.width * roi.height * 3) as c_long,
                        )
                    };
                    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                        return Err(Error::InvalidId(self.id.0));
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                        return Err(Error::CameraClosed);
                    } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                        return Err(Error::TimedOut);
                    }
                    *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                    let img: DynamicSerialImage = SerialImageBuffer::<u8>::from_vec(
                        roi.width as usize,
                        roi.height as usize,
                        data,
                    )
                    .unwrap()
                    .into();
                    img
                }
            };
            let mut meta = ImageMetaData::full_builder(
                self.get_bin_x(),
                self.get_bin_y(),
                self.roi.y_min,
                self.roi.x_min,
                self.get_temperature().unwrap_or(-273.0),
                self.exposure,
                start_time,
                self.camera_name(),
                self.get_gain_raw(),
                self.get_offset() as i64,
                self.get_min_gain().unwrap_or(0) as i32,
                self.get_max_gain().unwrap_or(0) as i32,
            );
            meta.add_extended_attrib(
                "DARK_FRAME",
                if !self.get_shutter_open().unwrap_or(false) {
                    "True"
                } else {
                    "False"
                },
            );
            img.set_metadata(meta);

            return Ok(img);
        }
    }

    /// Start exposing the detector and return.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::ExposureInProgress`]: Exposure in progress.
    ///  - [`cameraunit::Error::InvalidId`]: Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`]: Camera is closed.
    ///  - [`cameraunit::Error::GeneralError`]: Video capture mode active.
    ///
    fn start_exposure(&self) -> Result<(), Error> {
        let start_time: SystemTime;
        {
            let mut capturing = self.capturing.lock().unwrap();
            let stat = self.get_exposure_status()?;
            if stat == ASIExposureStatus::Working {
                *capturing = true;
                return Err(Error::ExposureInProgress);
            } else if stat == ASIExposureStatus::Failed {
                *capturing = false;
                warn!("Exposure failed, retrying");
            }
            *capturing = true;
            start_time = SystemTime::now();
            let res = unsafe {
                ASIStartExposure(
                    self.id.0,
                    if self.is_dark_frame {
                        ASI_BOOL_ASI_TRUE as i32
                    } else {
                        ASI_BOOL_ASI_FALSE as i32
                    },
                )
            };
            if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                *capturing = false;
                return Err(Error::InvalidId(self.id.0));
            } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                *capturing = false;
                return Err(Error::CameraClosed);
            } else if res == ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE as i32 {
                *capturing = false;
                return Err(Error::GeneralError("Video mode active".to_owned()));
            }
            *self.last_img_start.lock().unwrap() = start_time;
            Ok(())
        }
    }

    /// Check if an image is ready after [`CameraUnit_ASI::start_exposure()`].
    ///
    /// # Returns
    ///  - `false` if exposure is in progress.
    ///  - `true` if exposure is ready for download.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`]: Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`]: Camera is closed.
    ///  - [`cameraunit::Error::ExposureFailed`]: Exposure failed for unknown reason/camera
    /// still idle, indicating previous exposure did not start.
    fn image_ready(&self) -> Result<bool, Error> {
        let mut capturing = self.capturing.lock().unwrap();
        let stat = self.get_exposure_status()?;
        match stat {
            ASIExposureStatus::Working => Ok(false),
            ASIExposureStatus::Failed => {
                *capturing = false;
                Err(Error::ExposureFailed("Unknown error".to_string()))
            }
            ASIExposureStatus::Idle => {
                *capturing = false;
                Err(Error::ExposureFailed(
                    "Camera is idle. Was exposure started?".to_string(),
                ))
            }
            ASIExposureStatus::Success => Ok(true),
        }
    }

    /// Download an image captured using [`CameraUnit_ASI::start_exposure()`].
    ///
    /// # Errors
    ///  - [`cameraunit::Error::ExposureInProgress`]: Exposure in progress.
    ///  - [`cameraunit::Error::ExposureFailed`]: Exposure failed for unknown reason/camera
    /// still idle, indicating previous exposure did not start.
    ///  - [`cameraunit::Error::TimedOut`]: Exposure download timed out.
    ///  - [`cameraunit::Error::InvalidId`]: Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`]: Camera is closed.
    fn download_image(&self) -> Result<DynamicSerialImage, Error> {
        let mut capturing = self.capturing.lock().unwrap();
        let stat = self.get_exposure_status()?;
        match stat {
            ASIExposureStatus::Working => Err(Error::ExposureInProgress),
            ASIExposureStatus::Failed => {
                *capturing = false;
                Err(Error::ExposureFailed("Unknown error".to_string()))
            }
            ASIExposureStatus::Idle => {
                *capturing = false;
                Err(Error::ExposureFailed(
                    "Camera is idle. Was exposure started?".to_string(),
                ))
            }
            ASIExposureStatus::Success => {
                let roi = self.get_roi_format()?;
                let mut img = match roi.fmt {
                    ASIImageFormat::ImageRAW8 => {
                        let mut data = vec![0u8; (roi.width * roi.height) as usize];
                        let res = unsafe {
                            ASIGetDataAfterExp(
                                self.id.0,
                                data.as_mut_ptr() as *mut c_uchar,
                                (roi.width * roi.height) as c_long,
                            )
                        };
                        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                            return Err(Error::InvalidId(self.id.0));
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                            return Err(Error::CameraClosed);
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                            return Err(Error::TimedOut);
                        }
                        *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                        let img: DynamicSerialImage = SerialImageBuffer::<u8>::from_vec(
                            roi.width as usize,
                            roi.height as usize,
                            data,
                        )
                        .unwrap()
                        .into();
                        img
                    }
                    ASIImageFormat::ImageRAW16 => {
                        let mut data = vec![0u16; (roi.width * roi.height) as usize];
                        let res = unsafe {
                            ASIGetDataAfterExp(
                                self.id.0,
                                data.as_mut_ptr() as *mut c_uchar,
                                (roi.width * roi.height * 2) as c_long,
                            )
                        };
                        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                            return Err(Error::InvalidId(self.id.0));
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                            return Err(Error::CameraClosed);
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                            return Err(Error::TimedOut);
                        }
                        *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                        let img: DynamicSerialImage = SerialImageBuffer::<u16>::from_vec(
                            roi.width as usize,
                            roi.height as usize,
                            data,
                        )
                        .unwrap()
                        .into();
                        img
                    }
                    ASIImageFormat::ImageRGB24 => {
                        let mut data = vec![0u8; (roi.width * roi.height * 3) as usize];
                        let res = unsafe {
                            ASIGetDataAfterExp(
                                self.id.0,
                                data.as_mut_ptr() as *mut c_uchar,
                                (roi.width * roi.height * 3) as c_long,
                            )
                        };
                        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
                            return Err(Error::InvalidId(self.id.0));
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
                            return Err(Error::CameraClosed);
                        } else if res == ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as i32 {
                            return Err(Error::TimedOut);
                        }
                        *capturing = false; // whether the call succeeds or fails, we are not capturing anymore
                        let img: DynamicSerialImage = SerialImageBuffer::<u8>::from_vec(
                            roi.width as usize,
                            roi.height as usize,
                            data,
                        )
                        .unwrap()
                        .into();
                        img
                    }
                };
                let mut meta = ImageMetaData::full_builder(
                    self.get_bin_x(),
                    self.get_bin_y(),
                    self.roi.y_min,
                    self.roi.x_min,
                    self.get_temperature().unwrap_or(-273.0),
                    self.exposure,
                    *self.last_img_start.lock().unwrap(),
                    self.camera_name(),
                    self.get_gain_raw(),
                    self.get_offset() as i64,
                    self.get_min_gain().unwrap_or(0) as i32,
                    self.get_max_gain().unwrap_or(0) as i32,
                );
                meta.add_extended_attrib(
                    "DARK_FRAME",
                    if !self.get_shutter_open().unwrap_or(false) {
                        "True"
                    } else {
                        "False"
                    },
                );
                img.set_metadata(meta);
                Ok(img)
            }
        }
    }

    fn get_bin_x(&self) -> u32 {
        self.roi.bin_x
    }

    fn get_bin_y(&self) -> u32 {
        self.roi.bin_y
    }

    fn get_exposure(&self) -> Duration {
        self.exposure
    }

    /// Get the camera gain in percentage.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn get_gain(&self) -> f32 {
        let res = get_control_value(self.id.0, ASIControlType::Gain);
        if let Ok((val, _)) = res {
            return (val as f32 - self.gain_min as f32) * 100.0
                / (self.gain_max as f32 - self.gain_min as f32);
        }
        0.0
    }

    fn get_roi(&self) -> &ROI {
        &self.roi
    }

    /// Get the raw camera gain.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn get_gain_raw(&self) -> i64 {
        let res = get_control_value(self.id.0, ASIControlType::Gain);
        if let Ok((val, _)) = res {
            return val;
        }
        0
    }

    /// Get the pixel offset.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn get_offset(&self) -> i32 {
        let res = get_control_value(self.id.0, ASIControlType::Offset);
        if let Ok((val, _)) = res {
            return val as i32;
        }
        0
    }

    /// Get the shutter state.
    ///
    /// # Errors
    /// Does not return an error.
    fn get_shutter_open(&self) -> Result<bool, Error> {
        Ok(!self.is_dark_frame)
    }

    /// Set the camera exposure time.
    ///
    /// # Arguments
    ///  * `exposure` - Exposure time.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidValue`] - Exposure time is outside of valid range.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn set_exposure(&mut self, exposure: Duration) -> Result<Duration, Error> {
        if exposure < self.exp_min {
            return Err(Error::InvalidValue(format!(
                "Exposure {} us is below minimum of {} us",
                exposure.as_micros(),
                self.exp_min.as_micros()
            )));
        } else if exposure > self.exp_max {
            return Err(Error::InvalidValue(format!(
                "Exposure {} is above maximum of {}",
                exposure.as_secs_f32(),
                self.exp_max.as_secs_f32()
            )));
        }
        let capturing = self.capturing.lock().unwrap();
        if *capturing {
            return Err(Error::ExposureInProgress);
        }
        set_control_value(
            self.id.0,
            ASIControlType::Exposure,
            exposure.as_micros() as c_long,
            false,
        )?;
        let (exposure, _is_auto) = get_control_value(self.id.0, ASIControlType::Exposure)?;
        self.exposure = Duration::from_micros(exposure as u64);
        Ok(self.exposure)
    }

    /// Set the camera gain in percentage.
    ///
    /// # Arguments
    ///  * `gain` - Gain in percentage, must be between 0 and 100.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidValue`] - Gain is outside of valid range.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn set_gain(&mut self, gain: f32) -> Result<f32, Error> {
        if !(0.0..=100.0).contains(&gain) {
            return Err(Error::InvalidValue(format!(
                "Gain {} is outside of range 0-100",
                gain
            )));
        }
        let gain = (gain * (self.gain_max as f32 - self.gain_min as f32) / 100.0
            + self.gain_min as f32) as c_long;
        let gain = self.set_gain_raw(gain)?;
        Ok((gain as f32 - self.gain_min as f32) * 100.0
            / (self.gain_max as f32 - self.gain_min as f32))
    }

    /// Set the camera gain in raw values.
    ///
    /// # Arguments
    ///  * `gain` - Camera gain.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidValue`] - Gain is outside of valid range.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn set_gain_raw(&mut self, gain: i64) -> Result<i64, Error> {
        if gain < self.gain_min {
            return Err(Error::InvalidValue(format!(
                "Gain {} is below minimum of {}",
                gain, self.gain_min
            )));
        } else if gain > self.gain_max {
            return Err(Error::InvalidValue(format!(
                "Gain {} is above maximum of {}",
                gain, self.gain_max
            )));
        }
        let capturing = self.capturing.lock().unwrap();
        if *capturing {
            return Err(Error::ExposureInProgress);
        }
        set_control_value(self.id.0, ASIControlType::Gain, gain as c_long, false)?;
        Ok(self.get_gain_raw())
    }

    /// Set the region of interest.
    ///
    /// # Arguments
    ///  * `roi` - Region of interest, of type `cameraunit::ROI`.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidValue`] - ROI is invalid.
    ///  - [`cameraunit::Error::OutOfBounds`] - ROI is outside of the CCD.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    fn set_roi(&mut self, roi: &ROI) -> Result<&ROI, Error> {
        if roi.bin_x != roi.bin_y {
            return Err(Error::InvalidValue(
                "Bin X and Bin Y must be equal".to_owned(),
            ));
        }

        if roi.bin_x < 1 {
            return Err(Error::InvalidValue(format!(
                "Bin {} is below minimum of 1",
                roi.bin_x
            )));
        }

        if !self.props.supported_bins.contains(&roi.bin_x) {
            return Err(Error::InvalidValue(format!(
                "Bin {} is not supported by camera",
                roi.bin_x
            )));
        }

        let mut roi = *roi;
        roi.bin_y = roi.bin_x;

        if roi.width + roi.x_min > self.props.max_width / roi.bin_x {
            roi.width = (self.props.max_width - roi.x_min) / roi.bin_x;
        }
        if roi.height + roi.y_min > self.props.max_height / roi.bin_y {
            roi.height = (self.props.max_height - roi.y_min) / roi.bin_y;
        }

        roi.width -= roi.width % 8;
        roi.height -= roi.height % 2;

        if roi.width > self.props.max_width / self.roi.bin_x
            || roi.height > self.props.max_height / self.roi.bin_y
        {
            return Err(Error::InvalidValue(
                "ROI width and height must be positive".to_owned(),
            ));
        }

        if !self.props.is_usb3_camera
            && self.camera_name().contains("ASI120")
            && roi.width * roi.height % 1024 != 0
        {
            return Err(Error::InvalidValue(
                "ASI120 cameras require ROI width * height to be a multiple of 1024".to_owned(),
            ));
        }

        let capturing = self.capturing.lock().unwrap();
        if *capturing {
            return Err(Error::ExposureInProgress);
        }

        let mut roi_md = self.get_roi_format()?;
        let (_xs, _ys) = self.get_start_pos()?;
        let roi_md_old = roi_md.clone();

        roi_md.width = roi.width as i32;
        roi_md.height = roi.height as i32;
        roi_md.bin = roi.bin_x as i32;

        self.set_roi_format(&roi_md)?;

        if self
            .set_start_pos(roi.x_min as i32, roi.y_min as i32)
            .is_err()
        {
            self.set_roi_format(&roi_md_old)?;
        }
        self.roi = roi;
        Ok(&self.roi)
    }

    /// Set the shutter to open (always or during exposure) or closed.
    ///
    /// # Arguments
    ///  * `open` - Whether to open the shutter.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a mechanical shutter.
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    fn set_shutter_open(&mut self, open: bool) -> Result<bool, Error> {
        let capturing = self.capturing.lock().unwrap();
        if *capturing {
            return Err(Error::ExposureInProgress);
        }
        if !self.props.mechanical_shutter {
            return Err(Error::InvalidControlType(
                "Camera does not have mechanical shutter".to_owned(),
            ));
        }
        self.is_dark_frame = !open;
        Ok(open)
    }

    /// Flip the image along X and/or Y axes.
    ///
    /// # Arguments
    ///  * `x` - Whether to flip along the X axis.
    ///  * `y` - Whether to flip along the Y axis.
    ///
    /// # Errors
    ///  - [`cameraunit::Error::ExposureInProgress`] - An exposure is already in progress.
    ///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID.
    ///  - [`cameraunit::Error::CameraClosed`] - Camera is closed.
    ///  - [`cameraunit::Error::InvalidControlType`] - Camera does not support flipping.
    ///  - [`cameraunit::Error::Message`] - Camera does not support flipping.
    ///
    fn set_flip(&mut self, x: bool, y: bool) -> Result<(), Error> {
        let capturing = self.capturing.lock().unwrap();
        if *capturing {
            return Err(Error::ExposureInProgress);
        }
        let flipmode = {
            if x && y {
                ASI_FLIP_STATUS_ASI_FLIP_BOTH
            } else if x {
                ASI_FLIP_STATUS_ASI_FLIP_HORIZ
            } else if y {
                ASI_FLIP_STATUS_ASI_FLIP_VERT
            } else {
                ASI_FLIP_STATUS_ASI_FLIP_NONE
            }
        };
        set_control_value(self.id.0, ASIControlType::Flip, flipmode as c_long, false)
    }

    /// Check if the image is flipped along X and/or Y axes.
    ///
    /// # Returns
    ///  * `(x, y)` - Whether the image is flipped along the X and/or Y axes. Both `false` may indicate error getting the flip status.
    fn get_flip(&self) -> (bool, bool) {
        let (flipmode, _is_auto) = get_control_value(self.id.0, ASIControlType::Flip)
            .unwrap_or((ASI_FLIP_STATUS_ASI_FLIP_NONE as c_long, false));
        let flipmode = c_long::from(flipmode);
        let x = flipmode == ASI_FLIP_STATUS_ASI_FLIP_HORIZ as c_long
            || flipmode == ASI_FLIP_STATUS_ASI_FLIP_BOTH as c_long;
        let y = flipmode == ASI_FLIP_STATUS_ASI_FLIP_VERT as c_long
            || flipmode == ASI_FLIP_STATUS_ASI_FLIP_BOTH as c_long;
        (x, y)
    }

    fn set_bpp(&mut self, bpp: PixelBpp) -> Result<PixelBpp, Error> {
        match bpp {
            PixelBpp::Bpp8 => {
                self.set_image_fmt(ASIImageFormat::ImageRAW8)?;
                Ok(PixelBpp::Bpp8)
            }
            PixelBpp::Bpp16 => {
                self.set_image_fmt(ASIImageFormat::ImageRAW16)?;
                Ok(PixelBpp::Bpp16)
            }
            PixelBpp::Bpp24 => {
                self.set_image_fmt(ASIImageFormat::ImageRGB24)?;
                Ok(PixelBpp::Bpp24)
            }
            _ => Err(Error::InvalidValue(format!(
                "Invalid pixel bit depth {:?}",
                bpp
            ))),
        }
    }

    fn get_bpp(&self) -> cameraunit::PixelBpp {
        match self.get_image_fmt() {
            ASIImageFormat::ImageRAW8 => cameraunit::PixelBpp::Bpp8,
            ASIImageFormat::ImageRAW16 => cameraunit::PixelBpp::Bpp16,
            ASIImageFormat::ImageRGB24 => cameraunit::PixelBpp::Bpp24,
        }
    }
}

impl Default for ASIControlCaps {
    fn default() -> Self {
        ASIControlCaps {
            id: ASIControlType::Gain,
            name: [0; 64],
            description: [0; 128],
            min_value: 0,
            max_value: 0,
            default_value: 0,
            is_auto_supported: false,
            is_writable: false,
        }
    }
}

impl Display for ASIControlCaps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Control: {} - {:#?}\n\tDescription: {}\n\tRange: {} - {}\n\tDefault: {}\n\tAuto: {}, Writable: {}",
            string_from_char(&self.name),
            self.id,
            string_from_char(&self.description),
            self.min_value,
            self.max_value,
            self.default_value,
            self.is_auto_supported,
            self.is_writable,
        )
    }
}

impl ASIExposureStatus {
    fn from_u32(val: u32) -> Result<Self, Error> {
        match val {
            ASI_EXPOSURE_STATUS_ASI_EXP_IDLE => Ok(ASIExposureStatus::Idle),
            ASI_EXPOSURE_STATUS_ASI_EXP_WORKING => Ok(ASIExposureStatus::Working),
            ASI_EXPOSURE_STATUS_ASI_EXP_SUCCESS => Ok(ASIExposureStatus::Success),
            ASI_EXPOSURE_STATUS_ASI_EXP_FAILED => Ok(ASIExposureStatus::Failed),
            _ => Err(Error::InvalidMode(format!(
                "Invalid exposure status: {}",
                val
            ))),
        }
    }
}

impl Display for ASIExposureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ASIExposureStatus::Idle => write!(f, "Idle"),
            ASIExposureStatus::Working => write!(f, "Working"),
            ASIExposureStatus::Success => write!(f, "Success"),
            ASIExposureStatus::Failed => write!(f, "Failed"),
        }
    }
}

impl Display for ASICameraProps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Camera {}\n\tID: {} UUID: {}\n\tDetector: {} x {}\n\tColor: {}, Shutter: {}, Cooler: {}, USB3: {}, Trigger: {}\n\tBayer Pattern: {:#?}\n\tBins: {:?}\n\tPixel Size: {} um, e/ADU: {}, Bit Depth: {}
            ",
            self.name,
            self.id,
            String::from_utf8_lossy(&self.uuid),
            self.max_width,
            self.max_height,
            self.is_color_cam,
            self.mechanical_shutter,
            self.is_cooler_cam,
            self.is_usb3_camera,
            self.is_trigger_camera,
            self.bayer_pattern,
            self.supported_bins,
            self.pixel_size,
            self.e_per_adu,
            self.bit_depth,
        )
    }
}

impl ASIControlType {
    fn from_u32(val: u32) -> Option<Self> {
        match val {
            ASI_CONTROL_TYPE_ASI_GAIN => Some(ASIControlType::Gain),
            ASI_CONTROL_TYPE_ASI_EXPOSURE => Some(ASIControlType::Exposure),
            ASI_CONTROL_TYPE_ASI_GAMMA => Some(ASIControlType::Gamma),
            ASI_CONTROL_TYPE_ASI_WB_R => Some(ASIControlType::WhiteBalR),
            ASI_CONTROL_TYPE_ASI_WB_B => Some(ASIControlType::WhiteBalB),
            ASI_CONTROL_TYPE_ASI_OFFSET => Some(ASIControlType::Offset),
            ASI_CONTROL_TYPE_ASI_BANDWIDTHOVERLOAD => Some(ASIControlType::BWOvld),
            ASI_CONTROL_TYPE_ASI_OVERCLOCK => Some(ASIControlType::Overclock),
            ASI_CONTROL_TYPE_ASI_TEMPERATURE => Some(ASIControlType::Temperature),
            ASI_CONTROL_TYPE_ASI_FLIP => Some(ASIControlType::Flip),
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_GAIN => Some(ASIControlType::AutoExpMaxGain),
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_EXP => Some(ASIControlType::AutoExpMaxExp),
            ASI_CONTROL_TYPE_ASI_AUTO_TARGET_BRIGHTNESS => {
                Some(ASIControlType::AutoExpTgtBrightness)
            }
            ASI_CONTROL_TYPE_ASI_HARDWARE_BIN => Some(ASIControlType::HWBin),
            ASI_CONTROL_TYPE_ASI_HIGH_SPEED_MODE => Some(ASIControlType::HighSpeedMode),
            ASI_CONTROL_TYPE_ASI_COOLER_POWER_PERC => Some(ASIControlType::CoolerPowerPercent),
            ASI_CONTROL_TYPE_ASI_TARGET_TEMP => Some(ASIControlType::TargetTemp),
            ASI_CONTROL_TYPE_ASI_COOLER_ON => Some(ASIControlType::CoolerOn),
            ASI_CONTROL_TYPE_ASI_MONO_BIN => Some(ASIControlType::MonoBin),
            ASI_CONTROL_TYPE_ASI_FAN_ON => Some(ASIControlType::FanOn),
            ASI_CONTROL_TYPE_ASI_PATTERN_ADJUST => Some(ASIControlType::PatternAdjust),
            ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER => Some(ASIControlType::AntiDewHeater),
            _ => None,
        }
    }
}

impl ASIBayerPattern {
    fn from_u32(val: u32) -> Option<Self> {
        match val {
            ASI_BAYER_PATTERN_ASI_BAYER_RG => Some(ASIBayerPattern::BayerRG),
            ASI_BAYER_PATTERN_ASI_BAYER_BG => Some(ASIBayerPattern::BayerBG),
            ASI_BAYER_PATTERN_ASI_BAYER_GR => Some(ASIBayerPattern::BayerGR),
            ASI_BAYER_PATTERN_ASI_BAYER_GB => Some(ASIBayerPattern::BayerGB),
            _ => None,
        }
    }
}

impl ASIImageFormat {
    fn from_u32(val: u32) -> Option<Self> {
        match val as i32 {
            ASI_IMG_TYPE_ASI_IMG_RAW8 => Some(ASIImageFormat::ImageRAW8),
            ASI_IMG_TYPE_ASI_IMG_RGB24 => Some(ASIImageFormat::ImageRGB24),
            ASI_IMG_TYPE_ASI_IMG_RAW16 => Some(ASIImageFormat::ImageRAW16),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Debug, PartialEq, Clone, Copy)]
enum ASIBayerPattern {
    BayerRG = ASI_BAYER_PATTERN_ASI_BAYER_RG,
    BayerBG = ASI_BAYER_PATTERN_ASI_BAYER_BG,
    BayerGR = ASI_BAYER_PATTERN_ASI_BAYER_GR,
    BayerGB = ASI_BAYER_PATTERN_ASI_BAYER_GB,
}

#[repr(i32)]
#[derive(Debug, PartialEq, Clone, Copy)]
#[deny(missing_docs)]
/// Available image pixel formats for the ZWO ASI cameras.
pub enum ASIImageFormat {
    /// 8-bit raw image.
    ImageRAW8 = ASI_IMG_TYPE_ASI_IMG_RAW8,
    /// 24-bit RGB image.
    ImageRGB24 = ASI_IMG_TYPE_ASI_IMG_RGB24,
    /// 16-bit raw image.
    ImageRAW16 = ASI_IMG_TYPE_ASI_IMG_RAW16,
}

#[repr(i32)]
#[derive(Debug, PartialEq, Clone, Copy)]
enum ASIControlType {
    Gain = ASI_CONTROL_TYPE_ASI_GAIN as i32,
    Exposure = ASI_CONTROL_TYPE_ASI_EXPOSURE as i32,
    Gamma = ASI_CONTROL_TYPE_ASI_GAMMA as i32,
    WhiteBalR = ASI_CONTROL_TYPE_ASI_WB_R as i32,
    WhiteBalB = ASI_CONTROL_TYPE_ASI_WB_B as i32,
    Offset = ASI_CONTROL_TYPE_ASI_OFFSET as i32,
    BWOvld = ASI_CONTROL_TYPE_ASI_BANDWIDTHOVERLOAD as i32,
    Overclock = ASI_CONTROL_TYPE_ASI_OVERCLOCK as i32,
    Temperature = ASI_CONTROL_TYPE_ASI_TEMPERATURE as i32,
    Flip = ASI_CONTROL_TYPE_ASI_FLIP as i32,
    AutoExpMaxGain = ASI_CONTROL_TYPE_ASI_AUTO_MAX_GAIN as i32,
    AutoExpMaxExp = ASI_CONTROL_TYPE_ASI_AUTO_MAX_EXP as i32,
    AutoExpTgtBrightness = ASI_CONTROL_TYPE_ASI_AUTO_TARGET_BRIGHTNESS as i32,
    HWBin = ASI_CONTROL_TYPE_ASI_HARDWARE_BIN as i32,
    HighSpeedMode = ASI_CONTROL_TYPE_ASI_HIGH_SPEED_MODE as i32,
    CoolerPowerPercent = ASI_CONTROL_TYPE_ASI_COOLER_POWER_PERC as i32,
    TargetTemp = ASI_CONTROL_TYPE_ASI_TARGET_TEMP as i32,
    CoolerOn = ASI_CONTROL_TYPE_ASI_COOLER_ON as i32,
    MonoBin = ASI_CONTROL_TYPE_ASI_MONO_BIN as i32,
    FanOn = ASI_CONTROL_TYPE_ASI_FAN_ON as i32,
    PatternAdjust = ASI_CONTROL_TYPE_ASI_PATTERN_ADJUST as i32,
    AntiDewHeater = ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER as i32,
}

#[repr(u32)]
#[derive(Clone, PartialEq, Copy)]
enum ASIExposureStatus {
    Idle = ASI_EXPOSURE_STATUS_ASI_EXP_IDLE,
    Working = ASI_EXPOSURE_STATUS_ASI_EXP_WORKING,
    Success = ASI_EXPOSURE_STATUS_ASI_EXP_SUCCESS,
    Failed = ASI_EXPOSURE_STATUS_ASI_EXP_FAILED,
}

#[derive(Clone)]
struct ASIControlCaps {
    id: ASIControlType,
    name: [raw::c_char; 64],
    description: [raw::c_char; 128],
    min_value: i64,
    max_value: i64,
    default_value: i64,
    is_auto_supported: bool,
    is_writable: bool,
}

#[derive(Clone)]
struct ASIRoiMode {
    width: i32,
    height: i32,
    bin: i32,
    fmt: ASIImageFormat,
}

#[derive(Clone, PartialEq, PartialOrd, Eq)]
struct ASICamId(i32);

impl Drop for ASICamId {
    fn drop(&mut self) {
        let res = unsafe { ASIStopExposure(self.0) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            warn!("StopExp: Invalid camera ID: {}", self.0);
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            warn!("StopExp: Camera {} is closed", self.0);
        }
        let res = unsafe {
            ASISetControlValue(
                self.0,
                ASI_CONTROL_TYPE_ASI_COOLER_ON as i32,
                0,
                ASI_BOOL_ASI_FALSE as i32,
            )
        };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            warn!("CoolerOff: Invalid camera ID: {}", self.0);
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            warn!("CoolerOff: Camera {} is closed", self.0);
        } else if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE as i32 {
            warn!("CoolerOff: Invalid Control camera {}", self.0);
        } else if res == ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR as i32 {
            warn!("CoolerOff: General error camera {}", self.0);
        }
        let res = unsafe { ASICloseCamera(self.0) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            warn!("Invalid camera ID: {}", self.0);
        }
    }
}

fn get_control_caps(id: i32) -> Result<Vec<ASIControlCaps>, Error> {
    let mut num_caps: i32 = 0;
    let res = unsafe { ASIGetNumOfControls(id, &mut num_caps) };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
        return Err(Error::CameraClosed);
    }
    let mut caps = Vec::<ASIControlCaps>::with_capacity(num_caps as usize);

    for i in 0..num_caps {
        let cap = MaybeUninit::<ASI_CONTROL_CAPS>::zeroed();
        let mut cap = unsafe { cap.assume_init() };
        let res = unsafe { ASIGetControlCaps(id, i, &mut cap) };
        if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
            return Err(Error::InvalidId(id));
        } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
            return Err(Error::CameraClosed);
        }
        if cap.ControlType > ASIControlType::AntiDewHeater as u32 {
            break;
        }
        let cap = ASIControlCaps {
            id: ASIControlType::from_u32(cap.ControlType)
                .ok_or(Error::InvalidControlType(cap.ControlType.to_string()))?,
            name: cap.Name,
            description: cap.Description,
            min_value: cap.MinValue,
            max_value: cap.MaxValue,
            default_value: cap.DefaultValue,
            is_auto_supported: cap.IsAutoSupported == ASI_BOOL_ASI_TRUE,
            is_writable: cap.IsWritable == ASI_BOOL_ASI_TRUE,
        };
        caps.push(cap);
    }

    Ok(caps)
}

fn get_gain_minmax(caps: &Vec<ASIControlCaps>) -> (i64, i64) {
    get_controlcap_minmax(caps, ASIControlType::Gain).unwrap_or((0, 0))
}

fn get_exposure_minmax(caps: &Vec<ASIControlCaps>) -> (Duration, Duration) {
    let minmax = get_controlcap_minmax(caps, ASIControlType::Exposure);
    if let Some((min, max)) = minmax {
        return (
            Duration::from_micros(min as u64),
            Duration::from_micros(max as u64),
        );
    }
    (Duration::from_micros(1000_u64), Duration::from_secs(200))
}

fn get_controlcap_minmax(caps: &Vec<ASIControlCaps>, id: ASIControlType) -> Option<(i64, i64)> {
    for cap in caps {
        if cap.id == id {
            return Some((cap.min_value, cap.max_value));
        }
    }
    None
}

/// ZWO ASI camera internal implementation to cancel ongoing capture.
///
/// # Errors
///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
///  - [`cameraunit::Error::CameraClosed`] - Camera is closed
fn sys_cancel_capture(id: i32) -> Result<(), Error> {
    let res = unsafe { ASIStopExposure(id) };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
        return Err(Error::CameraClosed);
    }
    Ok(())
}

fn get_control_value(id: i32, ctyp: ASIControlType) -> Result<(c_long, bool), Error> {
    let mut val: c_long = 0;
    let mut auto_val: i32 = ASI_BOOL_ASI_FALSE as i32;
    let res = unsafe { ASIGetControlValue(id, ctyp as i32, &mut val, &mut auto_val) };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE as i32 {
        return Err(Error::InvalidControlType(format!("{:#?}", ctyp)));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
        return Err(Error::CameraClosed);
    }
    Ok((val, auto_val == ASI_BOOL_ASI_TRUE as i32))
}

/// Errors: InvalidId, InvalidControlType, CameraClosed, Message(Could not set value for type)
fn set_control_value(id: i32, ctyp: ASIControlType, val: c_long, auto: bool) -> Result<(), Error> {
    let res = unsafe {
        ASISetControlValue(
            id,
            ctyp as i32,
            val,
            if auto {
                ASI_BOOL_ASI_TRUE as i32
            } else {
                ASI_BOOL_ASI_FALSE as i32
            },
        )
    };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE as i32 {
        return Err(Error::InvalidControlType(format!("{:#?}", ctyp)));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
        return Err(Error::CameraClosed);
    } else if res == ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR as i32 {
        return Err(Error::Message(format!(
            "Could not set control value for type {:#?}",
            ctyp
        )));
    }
    Ok(())
}
/// ZWO ASI camera internal implementation to set camera temperature.
///
/// # Arguments
///  * `id` - Camera ID
///  * `temperature` - Target temperature in degrees Celsius, must be between -80 C and 20 C.
///  * `is_cooler_cam` - Whether the camera has a cooler.
///
/// # Errors
///  - [`cameraunit::Error::InvalidControlType`] - Camera does not have a cooler
///  - [`cameraunit::Error::InvalidValue`] - Temperature is outside of range
///  - [`cameraunit::Error::InvalidId`] - Invalid camera ID
fn set_temperature(id: i32, temperature: f32, is_cooler_cam: bool) -> Result<f32, Error> {
    if !is_cooler_cam {
        return Err(Error::InvalidControlType(
            "Camera does not have cooler".to_owned(),
        ));
    }
    if temperature < -80.0 {
        return Err(Error::InvalidValue(format!(
            "Temperature {} is below minimum of -80",
            temperature
        )));
    } else if temperature > 20.0 {
        return Err(Error::InvalidValue(format!(
            "Temperature {} is above maximum of 20",
            temperature
        )));
    }
    let temperature = temperature as c_long;
    set_control_value(id, ASIControlType::TargetTemp, temperature, false)?;
    let (temperature, _is_auto) = get_control_value(id, ASIControlType::TargetTemp)?;
    set_control_value(id, ASIControlType::CoolerOn, 1, false)?;
    Ok(temperature as f32)
}

fn get_cooler_power(id: i32) -> Option<f32> {
    let res = get_control_value(id, ASIControlType::CoolerPowerPercent);
    if let Ok((val, _)) = res {
        return Some(val as f32);
    }
    None
}

fn get_temperature(id: i32) -> Option<f32> {
    let res = get_control_value(id, ASIControlType::Temperature);
    if let Ok((val, _)) = res {
        return Some(val as f32 / 10.0);
    }
    None
}

fn get_camera_prop_by_id(id: i32) -> Result<ASI_CAMERA_INFO, Error> {
    let res = unsafe { ASIOpenCamera(id) };
    if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_REMOVED as i32 {
        warn!("Camera removed");
        return Err(Error::CameraRemoved);
    } else if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    }
    let info = MaybeUninit::<ASI_CAMERA_INFO>::zeroed();
    let mut info = unsafe { info.assume_init() };
    let res = unsafe { ASIGetCameraPropertyByID(id, &mut info) };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as i32 {
        return Err(Error::InvalidId(id));
    } else if res == ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as i32 {
        warn!("Camera closed");
        return Err(Error::CameraClosed);
    }
    Ok(info)
}

fn get_camera_prop_by_idx(idx: i32) -> Result<ASI_CAMERA_INFO, Error> {
    let info = MaybeUninit::<ASI_CAMERA_INFO>::zeroed();
    let mut info = unsafe { info.assume_init() };
    let res = unsafe { ASIGetCameraProperty(&mut info, idx) };
    if res == ASI_ERROR_CODE_ASI_ERROR_INVALID_INDEX as i32 {
        return Err(Error::InvalidId(idx));
    }
    Ok(info)
}

fn string_from_char<const N: usize>(inp: &[raw::c_char; N]) -> String {
    let mut str = String::from_utf8_lossy(&unsafe {
        std::mem::transmute_copy::<[raw::c_char; N], [u8; N]>(inp)
    })
    .to_string();
    str.retain(|c| c != '\0');
    str.trim().to_string()
}
