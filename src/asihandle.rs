#![allow(unused)]
use core::{panic, str};
use std::{
    cell::{Ref, RefCell},
    collections::HashMap,
    ffi::{c_long, CStr},
    fmt::{self, Display, Formatter},
    hash::Hash,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread::sleep,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    zwo_ffi::{
        ASICloseCamera, ASIGetCameraProperty, ASIGetCameraPropertyByID, ASIGetControlCaps,
        ASIGetControlValue, ASIGetDataAfterExp, ASIGetExpStatus, ASIGetID,
        ASIGetNumOfConnectedCameras, ASIGetNumOfControls, ASIGetSerialNumber, ASIInitCamera,
        ASIOpenCamera, ASISetControlValue, ASISetID, ASIStartExposure, ASIStopExposure,
        ASI_BAYER_PATTERN_ASI_BAYER_BG, ASI_BAYER_PATTERN_ASI_BAYER_GB,
        ASI_BAYER_PATTERN_ASI_BAYER_GR, ASI_BAYER_PATTERN_ASI_BAYER_RG, ASI_BOOL_ASI_FALSE,
        ASI_BOOL_ASI_TRUE, ASI_CAMERA_INFO, ASI_CONTROL_CAPS, ASI_CONTROL_TYPE_ASI_COOLER_ON,
        ASI_CONTROL_TYPE_ASI_FLIP, ASI_FLIP_STATUS_ASI_FLIP_BOTH, ASI_FLIP_STATUS_ASI_FLIP_HORIZ,
        ASI_FLIP_STATUS_ASI_FLIP_NONE, ASI_FLIP_STATUS_ASI_FLIP_VERT, ASI_ID, ASI_IMG_TYPE,
        ASI_IMG_TYPE_ASI_IMG_END, ASI_IMG_TYPE_ASI_IMG_RAW16, ASI_IMG_TYPE_ASI_IMG_RAW8,
    },
    zwo_ffi_wrapper::{
        get_bins, get_caps, get_control_caps, get_control_value, get_info, get_pixfmt,
        get_split_ctrl, map_control_cap, set_control_value, string_from_char, to_asibool,
        AsiControlType, AsiCtrl, AsiDeviceCtrl, AsiError, AsiExposureStatus, AsiHandle, AsiRoi,
        AsiSensorCtrl,
    },
    ASICALL,
};

use generic_camera::{
    AnalogCtrl, CustomName, DeviceCtrl, DigitalIoCtrl, ExposureCtrl, GenCam, GenCamCtrl,
    GenCamInfo, GenCamResult, PropertyError, PropertyValue, SensorCtrl,
};
use generic_camera::{
    GenCamDescriptor, GenCamError, GenCamPixelBpp, GenCamRoi, GenCamState, Property, PropertyLims,
};

use log::warn;
use refimage::ColorSpace;
use refimage::{DynamicImageData, GenericImage, ImageData};

pub(crate) fn get_asi_devs() -> Result<Vec<GenCamDescriptor>, AsiError> {
    fn get_sn(handle: i32) -> Option<String> {
        let mut sn = ASI_ID::default();
        ASICALL!(ASIGetSerialNumber(handle, &mut sn as _)).ok()?;
        let ret = unsafe { ASIGetSerialNumber(handle, &mut sn as _) };
        let sn = sn
            .id
            .iter()
            .fold(String::new(), |acc, &x| format!("{}{:02X}", acc, x));
        Some(sn)
    }

    let num_cameras = unsafe { ASIGetNumOfConnectedCameras() };
    let mut devs = Vec::with_capacity(num_cameras as _);
    for id in 0..num_cameras {
        let mut dev = ASI_CAMERA_INFO::default();
        if ASICALL!(ASIGetCameraProperty(&mut dev, id)).is_err() {
            continue;
        }
        if ASICALL!(ASIOpenCamera(dev.CameraID)).is_err() {
            continue;
        }
        let sn = get_sn(dev.CameraID).unwrap_or("Unknown".into());
        let mut dev: GenCamDescriptor = dev.into();
        dev.info.insert("Serial Number".to_string(), sn.into());
        devs.push(dev);
    }
    Ok(devs)
}

fn get_sn(handle: i32) -> Result<[u8; 16], AsiError> {
    let mut sn = ASI_ID::default();
    ASICALL!(ASIGetSerialNumber(handle, &mut sn as _))?;
    let sn = sn
        .id
        .iter()
        .fold(String::new(), |acc, &x| format!("{}{:02X}", acc, x));
    let mut out = [0u8; 16];
    out.copy_from_slice(sn.as_bytes());
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LastExposureInfo {
    pub tstamp: SystemTime,
    pub exposure: Duration,
    pub darkframe: bool,
    pub gain: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct AsiImager {
    // Root handle
    handle: Arc<AsiHandle>,
    // Core parts for GenCam
    serial: [u8; 16],
    name: [u8; 20],
    cspace: ColorSpace,               // Bayer pattern
    shutter_open: Option<AtomicBool>, // Shutter open/closed not available on GenCamInfo
    exposure: AtomicU64,
    exposure_auto: AtomicBool,
    gain: RefCell<Option<i64>>,
    roi: (GenCamRoi, GenCamPixelBpp),
    last_exposure: RefCell<Option<LastExposureInfo>>,
    imgstor: Vec<u16>,
    sensor_ctrl: AsiSensorCtrl,
    // Shared with GenCamInfo
    has_cooler: bool,
    capturing: Arc<AtomicBool>,
    info: Arc<GenCamDescriptor>, // cloned to GenCamInfo
    device_ctrl: Arc<AsiDeviceCtrl>,
    start: Arc<RwLock<Option<Instant>>>,
}

/// [`GenCamInfoAsi`] implements the [`GenCamInfo`] trait for ASI cameras.
///
/// # Examples
/// ```
///
/// use generic_camera::{GenCam, GenCamDriver};
/// use generic_camera_asi::{GenCamAsi, GenCamDriverAsi};
///
/// let mut drv = GenCamDriverAsi::default();
/// if let Ok(mut cam) = drv.connect_first_device() {
///    println!("Connected to camera: {}", cam.camera_name());
///    if let Some(info) = cam.info_handle() {
///         println!("Capturing: {}", info.is_capturing());
/// } else {
///     println!("No camera info available");
///   }
/// } else {
///   println!("No cameras available");
/// }
#[derive(Debug, Clone)]
pub struct GenCamInfoAsi {
    pub(crate) handle: Arc<AsiHandle>,
    pub(crate) serial: [u8; 16],
    pub(crate) name: [u8; 20],
    pub(crate) has_cooler: bool,
    pub(crate) capturing: Arc<AtomicBool>,
    pub(crate) info: Arc<GenCamDescriptor>,
    pub(crate) ctrl: Arc<AsiDeviceCtrl>,
    pub(crate) start: Arc<RwLock<Option<Instant>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureInfo {
    pub roi: AsiRoi,
    pub last_exposure: Option<LastExposureInfo>,
}

pub fn open_device(ginfo: &GenCamDescriptor) -> Result<AsiImager, GenCamError> {
    let handle = ginfo.id as _;
    let info = get_info(handle)?;
    let caps = get_control_caps(handle)?;
    let (sensor_ctrl, device_ctrl) = get_split_ctrl(&info, &caps);
    let roi = AsiRoi::get(handle).map_err(|e| match e {
        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })?;
    let bpp = match roi.fmt {
        ASI_IMG_TYPE_ASI_IMG_RAW8 => GenCamPixelBpp::Bpp8,
        ASI_IMG_TYPE_ASI_IMG_RAW16 => GenCamPixelBpp::Bpp16,
        _ => {
            return Err(GenCamError::GeneralError(format!(
                "ASI: Invalid pixel format: {}",
                roi.fmt
            )))
        }
    };
    let roi = GenCamRoi {
        x_min: roi.x as _,
        y_min: roi.y as _,
        width: roi.width as _,
        height: roi.height as _,
        bin_x: roi.bin as _,
        bin_y: roi.bin as _,
    };
    let sn = get_sn(handle).map_err(|e| match e {
        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })?;
    let sname = string_from_char(&info.Name);
    let sname_ref = sname.as_bytes();
    let mut name = [0u8; 20];
    let len = sname.len().min(20);
    name[..len].copy_from_slice(&sname_ref[..len]);
    let bayer = if info.IsColorCam == ASI_BOOL_ASI_TRUE as _ {
        match info.BayerPattern {
            ASI_BAYER_PATTERN_ASI_BAYER_BG => ColorSpace::Bggr,
            ASI_BAYER_PATTERN_ASI_BAYER_GB => ColorSpace::Gbrg,
            ASI_BAYER_PATTERN_ASI_BAYER_GR => ColorSpace::Grbg,
            ASI_BAYER_PATTERN_ASI_BAYER_RG => ColorSpace::Rggb,
            _ => ColorSpace::Gray,
        }
    } else {
        ColorSpace::Gray
    };
    let out = AsiImager {
        handle: Arc::new(handle.into()),
        serial: sn,
        name,
        cspace: bayer,
        has_cooler: info.IsCoolerCam == ASI_BOOL_ASI_TRUE as _,
        shutter_open: if info.MechanicalShutter == ASI_BOOL_ASI_TRUE as _ {
            Some(AtomicBool::new(false))
        } else {
            None
        },
        capturing: Arc::new(AtomicBool::new(false)),
        exposure: AtomicU64::new(0),
        exposure_auto: AtomicBool::new(false),
        gain: RefCell::new(None),
        roi: (roi, bpp),
        last_exposure: RefCell::new(None),
        imgstor: vec![0u16; (info.MaxHeight * info.MaxWidth) as _],
        sensor_ctrl,
        info: Arc::new(ginfo.clone()),
        device_ctrl: Arc::new(device_ctrl),
        start: Arc::new(RwLock::new(None)),
    };
    out.get_exposure()?;
    Ok(out)
}

impl AsiImager {
    pub(crate) fn get_temperature(&self) -> Result<f32, GenCamError> {
        let handle = self.handle.handle();
        let mut temp = 0;
        let (temp, _) = get_control_value(handle, AsiControlType::Temperature)?;
        Ok(temp as f32 * 0.1)
    }

    /// Set exposure to device and update internal state
    pub(crate) fn set_exposure(&self, exposure: Duration, auto: bool) -> Result<(), GenCamError> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureInProgress);
        }
        let handle = self.handle.handle();
        let value = exposure.as_micros() as _;
        let auto = if auto {
            ASI_BOOL_ASI_TRUE as _
        } else {
            ASI_BOOL_ASI_FALSE as _
        };
        set_control_value(handle, AsiControlType::Exposure, value, auto)?;
        self.get_exposure()?;
        Ok(())
    }

    /// Get exposure from device and update internal state
    pub(crate) fn get_exposure(&self) -> Result<(Duration, bool), GenCamError> {
        let handle = self.handle.handle();
        let (exposure, auto) = get_control_value(handle, AsiControlType::Exposure)?;
        self.exposure.store(exposure as _, Ordering::SeqCst);
        self.exposure_auto
            .store(auto == ASI_BOOL_ASI_TRUE as _, Ordering::SeqCst);
        Ok((
            Duration::from_micros(exposure as _),
            auto == ASI_BOOL_ASI_TRUE as _,
        ))
    }

    pub(crate) fn set_roi_raw(&mut self, roi: &AsiRoi) -> Result<(), GenCamError> {
        let handle = self.handle.handle();
        roi.set(handle).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            AsiError::InvalidControlType(src, args) => {
                GenCamError::InvalidControlType(format!("src: {src:?}, args: {args:?}"))
            }
            AsiError::InvalidImage(src, args) => {
                GenCamError::InvalidImageType(format!("src: {src:?}, args: {args:?}"))
            }
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        self.roi = roi.convert();
        Ok(())
    }

    pub(crate) fn get_gain(&self) -> Result<i64, GenCamError> {
        let handle = self.handle.handle();
        if let Ok(mut gainref) = self.gain.try_borrow_mut() {
            if let Some(gain) = *gainref {
                Ok(gain)
            } else {
                let (gain, _) = get_control_value(handle, AsiControlType::Gain)?;
                *gainref = Some(gain);
                Ok(gain)
            }
        } else {
            Err(GenCamError::AccessViolation)
        }
    }

    pub(crate) fn set_gain(&self, gain: i64) -> Result<(), GenCamError> {
        let handle = self.handle.handle();
        set_control_value(handle, AsiControlType::Gain, gain, ASI_BOOL_ASI_FALSE as _)?;
        if let Ok(mut gainref) = self.gain.try_borrow_mut() {
            *gainref = Some(gain);
            Ok(())
        } else {
            Err(GenCamError::AccessViolation)
        }
    }

    pub(crate) fn get_state(&self) -> Result<GenCamState, GenCamError> {
        let capturing = self.capturing.load(Ordering::SeqCst);
        // not currently capturing
        if !capturing {
            return Ok(GenCamState::Idle);
        }
        let stat = self.handle.state_raw()?;
        match stat {
            // currently capturing, but returned idle?
            AsiExposureStatus::Idle => {
                self.capturing.store(false, Ordering::SeqCst);
                Ok(GenCamState::Errored(GenCamError::ExposureNotStarted))
            }
            // currently capturing
            AsiExposureStatus::Working => {
                if let Ok(start) = self.start.read() {
                    start
                        .map(|t| {
                            let elapsed = t.elapsed();
                            GenCamState::Exposing(Some(elapsed))
                        })
                        .ok_or(GenCamError::ExposureNotStarted)
                } else {
                    Err(GenCamError::AccessViolation)
                }
            }
            // exposure finished
            AsiExposureStatus::Success => Ok(GenCamState::ExposureFinished),
            // exposure failed
            AsiExposureStatus::Failed => {
                self.capturing.store(false, Ordering::SeqCst);
                Err(GenCamError::ExposureFailed("".into()))
            }
        }
    }

    pub fn start_exposure(&self) -> Result<(), GenCamError> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureInProgress);
        }
        let handle = self.handle.handle();
        self.capturing.store(true, Ordering::SeqCst); // indicate we are capturing
                                                      // now we are capturing
        let darkframe = if let Some(open) = (&self.shutter_open) {
            !open.load(Ordering::SeqCst)
        } else {
            false
        };

        let mut last_exposure = LastExposureInfo {
            tstamp: SystemTime::now(),
            exposure: Duration::from_micros(self.exposure.load(Ordering::SeqCst)),
            darkframe,
            gain: self.get_gain().ok(),
        };
        if let Ok(mut start) = self.start.write() {
            *start = Some(Instant::now());
        } else {
            Err(GenCamError::AccessViolation)?;
        }
        ASICALL!(ASIStartExposure(handle, to_asibool(darkframe) as _)).map_err(|e| {
            self.capturing.store(false, Ordering::SeqCst);
            match e {
                AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
                AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
                _ => GenCamError::GeneralError(format!("{:?}", e)),
            }
        })?;
        let Ok(mut lexp) = self.last_exposure.try_borrow_mut() else {
            return Err(GenCamError::AccessViolation);
        };
        *lexp = Some(last_exposure);
        Ok(())
    }

    pub fn stop_exposure(&self) -> Result<(), GenCamError> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureNotStarted);
        }
        let handle = self.handle.handle();
        let res = ASICALL!(ASIStopExposure(handle)).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        });
        self.capturing.store(false, Ordering::SeqCst);
        res
    }

    pub fn download_image(&mut self) -> Result<GenericImage, GenCamError> {
        lazy_static::lazy_static! {
            static ref IMGCTR: AtomicU32 = AtomicU32::new(0);
        };
        // check if capturing, if not return error
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureNotStarted);
        }
        // capturing, check state
        let handle = self.handle.handle();
        let state = self.handle.state_raw()?;
        let temp = self.get_temperature().unwrap_or(-273.16);
        let (roi, bpp) = &self.roi;
        let mut expinfo = self
            .last_exposure
            .try_borrow_mut()
            .map_err(|_| GenCamError::AccessViolation)?;
        let expinfo = match state {
            AsiExposureStatus::Working => Err(GenCamError::ExposureInProgress),
            AsiExposureStatus::Failed => {
                self.capturing.store(false, Ordering::SeqCst);
                Err(GenCamError::ExposureFailed("".into()))
            }
            AsiExposureStatus::Idle => {
                self.capturing.store(false, Ordering::SeqCst);
                *expinfo = None;
                let _ = self
                    .start
                    .try_write()
                    .map_err(|_| GenCamError::AccessViolation)?
                    .take();
                Err(GenCamError::ExposureNotStarted)
            }
            AsiExposureStatus::Success => {
                let now = SystemTime::now();
                let Some(expinfo) = expinfo.take() else {
                    return Err(GenCamError::ExposureNotStarted);
                };
                let mut ptr = self.imgstor.as_mut_ptr();
                let len = self.imgstor.len();
                ASICALL!(ASIGetDataAfterExp(handle, ptr as _, len as _)).map_err(|e| {
                    self.capturing.store(false, Ordering::SeqCst);
                    match e {
                        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
                        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
                        AsiError::Timeout(_, _) => GenCamError::TimedOut,
                        _ => GenCamError::GeneralError(format!("{:?}", e)),
                    }
                })?;
                self.capturing.store(false, Ordering::SeqCst); // image has been downloaded
                Ok(expinfo)
            }
        }?;

        let width = roi.width as _;
        let height = roi.height as _;
        let ptr = &mut self.imgstor;
        let img: DynamicImageData = match bpp {
            GenCamPixelBpp::Bpp8 => {
                let ptr = bytemuck::try_cast_slice_mut(ptr)
                    .map_err(|e| GenCamError::InvalidFormat(format!("{:?}", e)))?;
                let img = ImageData::from_mut_ref(
                    &mut ptr[..(width * height)],
                    width,
                    height,
                    refimage::ColorSpace::Gray,
                )
                .map_err(|e| GenCamError::InvalidFormat(format!("{:?}", e)))?;
                DynamicImageData::U8(img)
            }
            GenCamPixelBpp::Bpp16 => {
                let img = ImageData::from_mut_ref(
                    &mut ptr[..(width * height)],
                    width,
                    height,
                    refimage::ColorSpace::Gray,
                )
                .map_err(|e| GenCamError::InvalidFormat(format!("{:?}", e)))?;
                DynamicImageData::U16(img)
            }
            _ => {
                return Err(GenCamError::GeneralError({
                    format!("Unexpected pixel format: {:?}", bpp)
                }));
            }
        };
        let mut img = GenericImage::new(expinfo.tstamp, img);
        let info = &(*self.info);
        img.insert_key(
            "IMGSER",
            (IMGCTR.fetch_add(1, Ordering::SeqCst), "Image serial number"),
        );
        img.insert_key("EXPOSURE", (expinfo.exposure, "Exposure time"));
        img.insert_key(
            "EXPTIME",
            (expinfo.exposure.as_secs_f64(), "Exposure time in seconds"),
        );
        img.insert_key(
            "IMAGETYP",
            (
                if expinfo.darkframe { "Dark" } else { "Light" },
                "Frame type",
            ),
        );
        img.insert_key("GAIN", (expinfo.gain.unwrap_or(0), "Gain"));
        img.insert_key("XOFFSET", (roi.x_min, "X offset"));
        img.insert_key("YOFFSET", (roi.y_min, "Y offset"));
        img.insert_key("XBINNING", (1, "X binning"));
        img.insert_key("YBINNING", (1, "Y binning"));
        img.insert_key("CCD-TEMP", (temp, "CCD temperature"));
        img.insert_key(
            "CAMERA",
            (
                str::from_utf8(&self.name)
                    .unwrap_or("")
                    .trim_end_matches(char::from(0)),
                "Camera name",
            ),
        );
        img.insert_key(
            "SERIAL",
            (
                str::from_utf8(&self.serial)
                    .unwrap_or("")
                    .trim_end_matches(char::from(0)),
                "Camera serial number",
            ),
        );
        if ColorSpace::Gray != self.cspace {
            img.insert_key(
                "BAYERPAT",
                (
                    match self.cspace {
                        ColorSpace::Bggr => "BGGR",
                        ColorSpace::Gbrg => "GBRG",
                        ColorSpace::Grbg => "GRBG",
                        ColorSpace::Rggb => "RGGB",
                        _ => "Unknown",
                    },
                    "Bayer pattern",
                ),
            );
            img.insert_key("XBAYOFF", (roi.x_min % 2, "X offset of Bayer pattern"));
            img.insert_key("YBAYOFF", (roi.y_min % 2, "Y offset of Bayer pattern"));
        }
        Ok(img)
    }

    pub fn get_property(&self, prop: &GenCamCtrl) -> Result<(PropertyValue, bool), GenCamError> {
        if !self.sensor_ctrl.contains(prop) | !self.device_ctrl.contains(prop) {
            return Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            });
        };
        match prop {
            GenCamCtrl::Device(_) => self.device_ctrl.get_value(&self.handle, prop),
            GenCamCtrl::Exposure(ExposureCtrl::ExposureTime) => {
                let (exp, auto) = self.get_exposure()?;
                Ok((PropertyValue::from(exp), auto))
            }
            GenCamCtrl::Sensor(SensorCtrl::PixelFormat) => {
                let val: GenCamPixelBpp = (self.roi.1);
                Ok((PropertyValue::PixelFmt(val), false))
            }
            GenCamCtrl::Sensor(SensorCtrl::ReverseX) => {
                let (flipx, _) = self.get_flip()?;
                Ok((PropertyValue::Bool(flipx), false))
            }
            GenCamCtrl::Sensor(SensorCtrl::ReverseY) => {
                let (_, flipy) = self.get_flip()?;
                Ok((PropertyValue::Bool(flipy), false))
            }
            GenCamCtrl::Sensor(SensorCtrl::ShutterMode) => {
                if let Some(open) = &self.shutter_open {
                    Ok((PropertyValue::Bool(open.load(Ordering::SeqCst)), false))
                } else {
                    Err(GenCamError::PropertyError {
                        control: *prop,
                        error: PropertyError::NotFound,
                    })
                }
            }
            _ => Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            }),
        }
    }

    pub fn set_property(
        &mut self,
        prop: &GenCamCtrl,
        value: &PropertyValue,
        auto: bool,
    ) -> Result<(), GenCamError> {
        if !self.sensor_ctrl.contains(prop) | !self.device_ctrl.contains(prop) {
            return Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            });
        };
        let (ctrl, lims) = match prop {
            GenCamCtrl::Device(_) => {
                return self.device_ctrl.set_value(&self.handle, prop, value, auto);
            }
            _ => self
                .sensor_ctrl
                .get_controller(prop)
                .ok_or(GenCamError::PropertyError {
                    control: *prop,
                    error: PropertyError::NotFound,
                })?,
        };
        lims.validate(value)
            .map_err(|e| GenCamError::PropertyError {
                control: *prop,
                error: e,
            })?;
        let handle = self.handle.handle();
        // handle the sensor controls that don't need lock
        match ctrl {
            AsiControlType::AutoExpMax
            | AsiControlType::AutoExpTarget
            | AsiControlType::AutoExpMaxGain => {
                let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                    control: *prop,
                    error: e,
                })?;
                return set_control_value(handle, *ctrl, val, auto as _);
            }
            _ => {
                // fall through
            }
        };
        // handle the sensor controls that need lock
        if self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureInProgress);
        }
        match prop {
            GenCamCtrl::Sensor(SensorCtrl::PixelFormat) => {
                if let PropertyValue::PixelFmt(fmt) = value {
                    let roi = AsiRoi::concat(&self.roi.0, *fmt);
                    self.set_roi_raw(&roi)?;
                    Ok(())
                } else {
                    Err(GenCamError::PropertyError {
                        control: *prop,
                        error: PropertyError::ValueNotSupported,
                    })
                }
            }
            GenCamCtrl::Sensor(SensorCtrl::ShutterMode) => {
                let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                    control: *prop,
                    error: e,
                })?;

                if let Some(open) = &self.shutter_open {
                    open.store(val, Ordering::SeqCst);
                    Ok(())
                } else {
                    Err(GenCamError::PropertyError {
                        control: *prop,
                        error: PropertyError::NotFound,
                    })
                }
            }
            GenCamCtrl::Analog(AnalogCtrl::Gain | AnalogCtrl::Gamma) => {
                let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                    control: *prop,
                    error: e,
                })?;
                set_control_value(handle, *ctrl, val, auto as _)
            }
            GenCamCtrl::Sensor(SensorCtrl::ReverseX | SensorCtrl::ReverseY) => {
                let (mut flipx, mut flipy) = self.get_flip()?;
                let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                    control: *prop,
                    error: e,
                })?;
                match prop {
                    GenCamCtrl::Sensor(SensorCtrl::ReverseX) => flipx = val,
                    GenCamCtrl::Sensor(SensorCtrl::ReverseY) => flipy = val,
                    _ => {
                        return Err(GenCamError::PropertyError {
                            control: *prop,
                            error: PropertyError::NotFound,
                        })
                    }
                }
                self.set_flip(flipx, flipy)
            }
            _ => Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            }),
        }
    }

    fn get_flip(&self) -> Result<(bool, bool), GenCamError> {
        let handle = self.handle.handle();
        let mut flip = Default::default();
        let mut auto = Default::default();
        ASICALL!(ASIGetControlValue(
            handle,
            ASI_CONTROL_TYPE_ASI_FLIP as _,
            &mut flip,
            &mut auto
        ))
        .map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            AsiError::InvalidControlType(src, args) => {
                GenCamError::InvalidControlType(format!("{src:?}(args: {args:?})"))
            }
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        let flip = flip as _;
        Ok(match flip {
            ASI_FLIP_STATUS_ASI_FLIP_NONE => (false, false),
            ASI_FLIP_STATUS_ASI_FLIP_HORIZ => (true, false),
            ASI_FLIP_STATUS_ASI_FLIP_VERT => (false, true),
            ASI_FLIP_STATUS_ASI_FLIP_BOTH => (true, true),
            _ => {
                return Err(GenCamError::GeneralError(format!(
                    "ASI: Invalid flip status: {}",
                    flip
                )))
            }
        })
    }

    fn set_flip(&self, flipx: bool, flipy: bool) -> Result<(), GenCamError> {
        let handle = self.handle.handle();
        let flip = match (flipx, flipy) {
            (false, false) => ASI_FLIP_STATUS_ASI_FLIP_NONE,
            (true, false) => ASI_FLIP_STATUS_ASI_FLIP_HORIZ,
            (false, true) => ASI_FLIP_STATUS_ASI_FLIP_VERT,
            (true, true) => ASI_FLIP_STATUS_ASI_FLIP_BOTH,
        };
        ASICALL!(ASISetControlValue(
            handle,
            ASI_CONTROL_TYPE_ASI_FLIP as _,
            flip as _,
            ASI_BOOL_ASI_FALSE as i32
        ))
        .map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            AsiError::InvalidControlType(src, args) => {
                GenCamError::InvalidControlType(format!("{src:?}(args: {args:?})"))
            }
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        Ok(())
    }

    pub fn image_ready(&self) -> GenCamResult<bool> {
        if !self.capturing.load(Ordering::SeqCst) {
            Err(GenCamError::ExposureNotStarted)
        } else {
            Ok(self.handle.state_raw()? == AsiExposureStatus::Success)
        }
    }

    pub fn camera_name(&self) -> &str {
        str::from_utf8(&self.name)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }

    pub fn set_roi(&mut self, roi: &GenCamRoi) -> Result<&GenCamRoi, GenCamError> {
        if self.is_capturing() {
            return Err(GenCamError::ExposureInProgress);
        }
        let roi = AsiRoi::concat(roi, self.roi.1);
        self.set_roi_raw(&roi)?;
        Ok(&self.roi.0)
    }

    pub fn get_roi(&self) -> &GenCamRoi {
        &self.roi.0
    }

    pub fn get_concat_caps(&self) -> HashMap<GenCamCtrl, Property> {
        let mut out = self.sensor_ctrl.list_properties().clone();
        out.extend(self.device_ctrl.list_properties().clone());
        out
    }

    pub fn get_info_handle(&self) -> GenCamInfoAsi {
        GenCamInfoAsi {
            handle: self.handle.clone(),
            serial: self.serial,
            name: self.name,
            has_cooler: self.has_cooler,
            capturing: self.capturing.clone(),
            info: self.info.clone(),
            ctrl: self.device_ctrl.clone(),
            start: self.start.clone(),
        }
    }
}

impl GenCamInfo for GenCamInfoAsi {
    fn camera_ready(&self) -> bool {
        true
    }

    fn camera_name(&self) -> &str {
        str::from_utf8(&self.name)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
    }

    fn cancel_capture(&self) -> GenCamResult<()> {
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureNotStarted);
        }
        let handle = self.handle.handle();
        let res = ASICALL!(ASIStopExposure(handle)).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        });
        self.capturing.store(false, Ordering::SeqCst);
        res
    }

    fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }

    fn camera_state(&self) -> GenCamResult<GenCamState> {
        let capturing = self.capturing.load(Ordering::SeqCst);
        // not currently capturing
        if !capturing {
            return Ok(GenCamState::Idle);
        }
        let stat = self.handle.state_raw()?;
        match stat {
            // currently capturing, but returned idle?
            AsiExposureStatus::Idle => {
                self.capturing.store(false, Ordering::SeqCst);
                Ok(GenCamState::Errored(GenCamError::ExposureNotStarted))
            }
            // currently capturing
            AsiExposureStatus::Working => {
                if let Ok(start) = self.start.read() {
                    start
                        .map(|t| {
                            let elapsed = t.elapsed();
                            GenCamState::Exposing(Some(elapsed))
                        })
                        .ok_or(GenCamError::ExposureNotStarted)
                } else {
                    Err(GenCamError::AccessViolation)
                }
            }
            // exposure finished
            AsiExposureStatus::Success => Ok(GenCamState::ExposureFinished),
            // exposure failed
            AsiExposureStatus::Failed => {
                self.capturing.store(false, Ordering::SeqCst);
                Err(GenCamError::ExposureFailed("".into()))
            }
        }
    }

    fn list_properties(&self) -> &HashMap<GenCamCtrl, Property> {
        self.ctrl.list_properties()
    }

    fn get_property(&self, name: GenCamCtrl) -> GenCamResult<(PropertyValue, bool)> {
        if !self.ctrl.contains(&name) {
            return Err(GenCamError::PropertyError {
                control: name,
                error: PropertyError::NotFound,
            });
        };
        self.ctrl.get_value(&self.handle, &name)
    }

    fn set_property(
        &mut self,
        name: GenCamCtrl,
        value: &PropertyValue,
        auto: bool,
    ) -> GenCamResult<()> {
        if !self.ctrl.contains(&name) {
            return Err(GenCamError::PropertyError {
                control: name,
                error: PropertyError::NotFound,
            });
        };
        self.ctrl.set_value(&self.handle, &name, value, auto)
    }
}
