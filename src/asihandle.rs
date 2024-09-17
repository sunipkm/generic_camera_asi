#![allow(unused)]
use core::{panic, str};
use std::{
    cell::{Ref, RefCell},
    collections::HashMap,
    ffi::{c_long, CStr},
    fmt::{self, Display, Formatter},
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
        ASICloseCamera, ASIGetCameraProperty, ASIGetCameraPropertyByID, ASIGetControlCaps, ASIGetControlValue, ASIGetDataAfterExp, ASIGetExpStatus, ASIGetID, ASIGetNumOfConnectedCameras, ASIGetNumOfControls, ASIGetSerialNumber, ASIInitCamera, ASIOpenCamera, ASISetControlValue, ASISetID, ASIStartExposure, ASIStopExposure, ASI_BAYER_PATTERN_ASI_BAYER_BG, ASI_BAYER_PATTERN_ASI_BAYER_GB, ASI_BAYER_PATTERN_ASI_BAYER_GR, ASI_BAYER_PATTERN_ASI_BAYER_RG, ASI_BOOL_ASI_FALSE, ASI_BOOL_ASI_TRUE, ASI_CAMERA_INFO, ASI_CONTROL_CAPS, ASI_CONTROL_TYPE_ASI_COOLER_ON, ASI_CONTROL_TYPE_ASI_FLIP, ASI_FLIP_STATUS_ASI_FLIP_BOTH, ASI_FLIP_STATUS_ASI_FLIP_HORIZ, ASI_FLIP_STATUS_ASI_FLIP_NONE, ASI_FLIP_STATUS_ASI_FLIP_VERT, ASI_ID, ASI_IMG_TYPE, ASI_IMG_TYPE_ASI_IMG_END, ASI_IMG_TYPE_ASI_IMG_RAW16, ASI_IMG_TYPE_ASI_IMG_RAW8
    },
    zwo_ffi_wrapper::{
        get_bins, get_control_value, get_pixfmt, map_control_cap, set_control_value,
        string_from_char, to_asibool, AsiControlType, AsiError, AsiExposureStatus, AsiRoi,
    },
    ASICALL,
};

use generic_camera::{
    AnalogCtrl, CustomName, DeviceCtrl, DigitalIoCtrl, ExposureCtrl, GenCam, GenCamCtrl, GenCamResult, PropertyError, PropertyValue, SensorCtrl
};
use generic_camera::{
    GenCamDescriptor, GenCamError, GenCamPixelBpp, GenCamRoi, GenCamState, Property, PropertyLims,
};

use log::warn;
use refimage::{DynamicImageData, GenericImage, ImageData};

pub(crate) fn get_asi_devs() -> Result<Vec<GenCamDescriptor>, AsiError> {
    fn get_sn(handle: i32) -> Option<String> {
        ASICALL!(ASIOpenCamera(handle)).ok()?;
        let mut sn = ASI_ID::default();
        ASICALL!(ASIGetSerialNumber(handle, &mut sn as _)).ok()?;
        let ret = unsafe { ASIGetSerialNumber(handle, &mut sn as _) };
        unsafe { ASICloseCamera(handle) };
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
pub(crate) struct AsiHandle {
    handle: i32,
    serial: [u8; 16],
    name: [u8; 20],
    bayer: Option<[u8; 4]>, // Bayer pattern
    has_cooler: bool,
    shutter_open: Option<Arc<AtomicBool>>,
    capturing: Arc<AtomicBool>,
    exposure: AtomicU64,
    gain: RefCell<Option<i64>>,
    info: Arc<ASI_CAMERA_INFO>, // cloned to GenCamInfo
    caps: Box<HashMap<GenCamCtrl, (AsiControlType, Property)>>, // truncated and copied to GenCamInfo
    roi: RefCell<AsiRoi>,
    last_exposure: RefCell<Option<LastExposureInfo>>,
    start: Arc<RwLock<Option<Instant>>>,
    imgstor: Vec<u16>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureInfo {
    pub roi: AsiRoi,
    pub last_exposure: Option<LastExposureInfo>,
}

impl Drop for AsiHandle {
    fn drop(&mut self) {
        let handle = self.handle;
        if let Err(e) = ASICALL!(ASIStopExposure(handle)) {
            warn!("Failed to stop exposure: {:?}", e);
        }
        if self.has_cooler {
            if let Err(e) = ASICALL!(ASISetControlValue(
                handle,
                ASI_CONTROL_TYPE_ASI_COOLER_ON as i32,
                0,
                ASI_BOOL_ASI_FALSE as i32
            )) {
                warn!("Failed to turn off cooler: {:?}", e);
            }
        }
        if let Err(e) = ASICALL!(ASICloseCamera(handle)) {
            warn!("Failed to close camera: {:?}", e);
        }
    }
}

fn get_info(handle: i32) -> Result<ASI_CAMERA_INFO, GenCamError> {
    let mut info = ASI_CAMERA_INFO::default();
    ASICALL!(ASIGetCameraPropertyByID(handle, &mut info)).map_err(|e| match e {
        AsiError::CameraRemoved(_, _) => GenCamError::CameraRemoved,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })?;
    Ok(info)
}

pub(crate) fn get_control_caps(handle: i32) -> Result<Vec<ASI_CONTROL_CAPS>, GenCamError> {
    let mut num_ctrl = 0;
    ASICALL!(ASIGetNumOfControls(handle, &mut num_ctrl)).map_err(|e| match e {
        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })?;
    let mut caps = Vec::with_capacity(num_ctrl as _);
    for i in 0..num_ctrl {
        let mut cap = ASI_CONTROL_CAPS::default();
        if let Some(e) = ASICALL!(ASIGetControlCaps(handle, i, &mut cap)).err() {
            match e {
                AsiError::CameraClosed(_, _) => return Err(GenCamError::CameraClosed),
                AsiError::InvalidId(_, _) => return Err(GenCamError::InvalidId(handle)),
                _ => continue,
            }
        };
        caps.push(cap);
    }
    Ok(caps)
}

impl AsiHandle {
    /// Create a new AsiHandle from a camera handle
    /// Removed binning support for now
    pub(crate) fn new(handle: i32) -> Result<Self, GenCamError> {
        ASICALL!(ASIOpenCamera(handle)).map_err(|e| match e {
            AsiError::CameraRemoved(_, _) => GenCamError::CameraRemoved,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        let info = get_info(handle)?;
        let caps = get_control_caps(handle)?;
        let mut caps: HashMap<GenCamCtrl, (AsiControlType, Property)> =
            caps.iter().filter_map(map_control_cap).collect();
        caps.insert(
            SensorCtrl::PixelFormat.into(),
            (
                AsiControlType::Invalid,
                Property::new(
                    PropertyLims::PixelFmt {
                        variants: get_pixfmt(
                            &info.SupportedVideoFormat,
                            ASI_IMG_TYPE_ASI_IMG_END as _,
                        ),
                        default: get_pixfmt(
                            &info.SupportedVideoFormat,
                            ASI_IMG_TYPE_ASI_IMG_END as _,
                        )[0], // Safety: get_pixfmt() returns at least one element
                    },
                    false,
                    false,
                ),
            ),
        );
        if info.IsUSB3Camera == ASI_BOOL_ASI_TRUE as _ {
            caps.insert(
                DeviceCtrl::Custom("UUID".into()).into(),
                (
                    AsiControlType::Invalid,
                    Property::new(
                        PropertyLims::EnumStr {
                            variants: Vec::new(),
                            default: "".into(),
                        },
                        false,
                        false,
                    ),
                ),
            );
        }
        caps.insert(
            SensorCtrl::ReverseX.into(),
            (
                AsiControlType::Flip,
                Property::new(PropertyLims::Bool { default: false }, false, false),
            ),
        );
        caps.insert(
            SensorCtrl::ReverseY.into(),
            (
                AsiControlType::Flip,
                Property::new(PropertyLims::Bool { default: false }, false, false),
            ),
        );
        if info.MechanicalShutter == ASI_BOOL_ASI_TRUE as _ {
            let mut prop = Property::new(PropertyLims::Bool { default: true }, false, false);
            prop.set_doc(
                "True if the shutter is open, false if the shutter is closed. Setting this property will open or close the shutter."
            );
            caps.insert(
                SensorCtrl::ShutterMode.into(),
                (AsiControlType::Invalid, prop),
            );
        }
        ASICALL!(ASIInitCamera(handle)).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        let roi = AsiRoi::get(handle).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
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
            let mut bayer = [0u8; 4];
            let val = match info.BayerPattern {
                ASI_BAYER_PATTERN_ASI_BAYER_BG => Some("GRBG"),
                ASI_BAYER_PATTERN_ASI_BAYER_GB => Some("RGGB"),
                ASI_BAYER_PATTERN_ASI_BAYER_GR => Some("BGGR"),
                ASI_BAYER_PATTERN_ASI_BAYER_RG => Some("GBRG"),
                _ => None,
            };
            if let Some(val) = val {
                bayer[..4].copy_from_slice(&val.as_bytes()[..4]);
                Some(bayer)
            } else {
                None
            }
        } else {
            None
        };
        let out = Self {
            handle,
            serial: sn,
            name,
            bayer,
            has_cooler: info.IsCoolerCam == ASI_BOOL_ASI_TRUE as _,
            shutter_open: if info.MechanicalShutter == ASI_BOOL_ASI_TRUE as _ {
                Some(Arc::new(AtomicBool::new(false)))
            } else {
                None
            },
            capturing: Arc::new(AtomicBool::new(false)),
            caps: Box::new(caps),
            exposure: AtomicU64::new(0),
            roi: RefCell::new(roi),
            last_exposure: RefCell::new(None),
            info: Arc::new(info),
            gain: RefCell::new(None),
            imgstor: vec![0u16; (info.MaxHeight * info.MaxWidth) as _],
            start: Arc::new(RwLock::new(None)),
        };
        out.get_exposure()?;
        Ok(out)
    }

    pub(crate) fn get_temperature(&self) -> Result<f32, GenCamError> {
        let handle = self.handle;
        let mut temp = 0;
        let (temp, _) = get_control_value(handle, AsiControlType::Temperature)?;
        Ok(temp as f32 * 0.1)
    }

    /// Set exposure to device and update internal state
    pub(crate) fn set_exposure(&self, exposure: Duration) -> Result<(), GenCamError> {
        if self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureInProgress);
        }
        let handle = self.handle;
        let value = exposure.as_micros() as _;
        set_control_value(
            handle,
            AsiControlType::Exposure,
            value,
            ASI_BOOL_ASI_FALSE as _,
        )?;
        self.exposure.store(value as _, Ordering::SeqCst);
        Ok(())
    }

    /// Get exposure from device and update internal state
    pub(crate) fn get_exposure(&self) -> Result<Duration, GenCamError> {
        let handle = self.handle;
        let (exposure, _) = get_control_value(handle, AsiControlType::Exposure)?;
        self.exposure.store(exposure as _, Ordering::SeqCst);
        Ok(Duration::from_micros(exposure as _))
    }

    /// Get ROI from device and update internal state
    pub(crate) fn get_roi(&self) -> Result<AsiRoi, GenCamError> {
        let res = AsiRoi::get(self.handle).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        if let Ok(mut roiref) = self.roi.try_borrow_mut() {
            *roiref = res;
            Ok(res)
        } else {
            Err(GenCamError::AccessViolation)
        }
    }

    pub(crate) fn set_roi(&self, roi: &AsiRoi) -> Result<(), GenCamError> {
        roi.set(self.handle).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
            AsiError::InvalidControlType(src, args) => {
                GenCamError::InvalidControlType(format!("src: {src:?}, args: {args:?}"))
            }
            AsiError::InvalidImage(src, args) => {
                GenCamError::InvalidImageType(format!("src: {src:?}, args: {args:?}"))
            }
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        if let Ok(mut roiref) = self.roi.try_borrow_mut() {
            *roiref = *roi;
            Ok(())
        } else {
            Err(GenCamError::AccessViolation)
        }
    }

    pub(crate) fn get_gain(&self) -> Result<i64, GenCamError> {
        let handle = self.handle;
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
        let handle = self.handle;
        set_control_value(handle, AsiControlType::Gain, gain, ASI_BOOL_ASI_FALSE as _)?;
        if let Ok(mut gainref) = self.gain.try_borrow_mut() {
            *gainref = Some(gain);
            Ok(())
        } else {
            Err(GenCamError::AccessViolation)
        }
    }

    pub(crate) fn get_state_raw(&self) -> Result<AsiExposureStatus, GenCamError> {
        let handle = self.handle;
        let mut stat = Default::default();
        ASICALL!(ASIGetExpStatus(handle, &mut stat)).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        Ok(stat.into())
    }

    pub(crate) fn get_state(&self) -> Result<GenCamState, GenCamError> {
        let handle = self.handle;
        let capturing = self.capturing.load(Ordering::SeqCst);
        // not currently capturing
        if !capturing {
            return Ok(GenCamState::Idle);
        }
        let stat = self.get_state_raw()?;
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
        let handle = self.handle;
        self.capturing.store(true, Ordering::SeqCst); // indicate we are capturing
                                                      // now we are capturing
        let darkframe = if let Some(open) = (&self.shutter_open) {
            !open.load(Ordering::SeqCst)
        } else {
            false
        };

        let Ok(roi) = self.roi.try_borrow() else {
            return Err(GenCamError::AccessViolation);
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
        let handle = self.handle;
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
        let handle = self.handle;
        let state = self.get_state_raw()?;
        let temp = self.get_temperature().unwrap_or(-273.16);
        let roi = self
            .roi
            .try_borrow()
            .map_err(|_| GenCamError::AccessViolation)?;
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
                let _ = self.start
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
        let img: DynamicImageData = match (roi.fmt as u32).into() {
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
                    format!("Unexpected pixel format: {:?}", roi.fmt)
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
        img.insert_key("XOFFSET", (roi.x, "X offset"));
        img.insert_key("YOFFSET", (roi.y, "Y offset"));
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
        if let Some(bayer) = &self.bayer {
            img.insert_key(
                "BAYERPAT",
                (
                    str::from_utf8(bayer)
                        .unwrap_or("")
                        .trim_end_matches(char::from(0)),
                    "Bayer pattern",
                ),
            );
            img.insert_key("XBAYOFF", (roi.x % 2, "X offset of Bayer pattern"));
            img.insert_key("YBAYOFF", (roi.y % 2, "Y offset of Bayer pattern"));
        }
        Ok(img)
    }

    pub fn get_property(&self, prop: &GenCamCtrl) -> Result<(PropertyValue, bool), GenCamError> {
        if let Some((ctrl, _)) = self.caps.get(prop) {
            if ctrl == &AsiControlType::Invalid {
                // special cases
                match prop {
                    GenCamCtrl::Sensor(SensorCtrl::PixelFormat) => {
                        let val: GenCamPixelBpp = (self.get_roi()?.fmt as u32).into();
                        Ok((PropertyValue::PixelFmt(val), false))
                    }
                    GenCamCtrl::Device(DeviceCtrl::Custom(name)) => {
                        if name.as_str() == "UUID" {
                            let mut sn = Default::default();
                            ASICALL!(ASIGetID(self.handle, &mut sn)).map_err(|e| match e {
                                AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
                                AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
                                _ => GenCamError::GeneralError(format!("{:?}", e)),
                            })?;
                            let id = str::from_utf8(&sn.id)
                                .map_err(|e| GenCamError::GeneralError(format!("{:?}", e)))?
                                .trim_end_matches(char::from(0));
                            Ok((PropertyValue::from(id), false))
                        } else {
                            Err(GenCamError::PropertyError {
                                control: *prop,
                                error: PropertyError::NotFound,
                            })
                        }
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
            } else {
                let handle = self.handle;
                let (val, auto) = get_control_value(handle, *ctrl)?;
                if ctrl == &AsiControlType::Temperature {
                    Ok((
                        PropertyValue::from(val as f64 * 0.1),
                        auto == ASI_BOOL_ASI_TRUE as _,
                    ))
                } else {
                    Ok((PropertyValue::from(val), auto == ASI_BOOL_ASI_TRUE as _))
                }
            }
        } else {
            Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            })
        }
    }

    pub fn set_property(
        &self,
        prop: &GenCamCtrl,
        value: &PropertyValue,
        auto: bool,
    ) -> Result<(), GenCamError> {
        let auto: i32 = if auto {
            ASI_BOOL_ASI_TRUE as _
        } else {
            ASI_BOOL_ASI_FALSE as _
        };
        if let Some((ctrl, lims)) = self.caps.get(prop) {
            if ctrl == &AsiControlType::Invalid {
                // special cases
                match prop {
                    GenCamCtrl::Sensor(SensorCtrl::PixelFormat) => {
                        lims.validate(value)
                            .map_err(|e| GenCamError::PropertyError {
                                control: *prop,
                                error: e,
                            })?;
                        if let PropertyValue::PixelFmt(fmt) = value {
                            if self.capturing.load(Ordering::SeqCst) {
                                return Err(GenCamError::ExposureInProgress);
                            }
                            let mut roi = self.get_roi()?;
                            let fmt = match fmt {
                                GenCamPixelBpp::Bpp8 => ASI_IMG_TYPE_ASI_IMG_RAW8,
                                GenCamPixelBpp::Bpp16 => ASI_IMG_TYPE_ASI_IMG_RAW16,
                                _ => {
                                    return Err(GenCamError::PropertyError {
                                        control: *prop,
                                        error: PropertyError::ValueNotSupported,
                                    })
                                }
                            };
                            roi.fmt = fmt as _;
                            self.set_roi(&roi)?;
                            Ok(())
                        } else {
                            Err(GenCamError::PropertyError {
                                control: *prop,
                                error: PropertyError::ValueNotSupported,
                            })
                        }
                    }
                    GenCamCtrl::Device(DeviceCtrl::Custom(name)) => {
                        if name.as_str() == "UUID" {
                            let mut sn: ASI_ID = Default::default();
                            let value: String = value
                                .try_into()
                                .map_err(|e| GenCamError::PropertyError {
                                    control: *prop,
                                    error: e,
                                })?;

                            let len = value.len().min(8);
                            sn.id[..len].copy_from_slice(&value.as_bytes()[..len]);
                            
                            ASICALL!(ASISetID(self.handle, sn)).map_err(|e| match e {
                                AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
                                AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
                                _ => GenCamError::GeneralError(format!("{:?}", e)),
                            })
                        } else {
                            Err(GenCamError::PropertyError {
                                control: *prop,
                                error: PropertyError::NotFound,
                            })
                        }
                    }
                    GenCamCtrl::Sensor(SensorCtrl::ShutterMode) => {
                        if self.capturing.load(Ordering::SeqCst) {
                            return Err(GenCamError::ExposureInProgress);
                        }
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
                    _ => Err(GenCamError::PropertyError {
                        control: *prop,
                        error: PropertyError::NotFound,
                    }),
                }
            } else {
                let handle = self.handle;
                match ctrl {
                    AsiControlType::Gain | AsiControlType::Gamma => {
                        if self.capturing.load(Ordering::SeqCst) {
                            Err(GenCamError::ExposureInProgress)
                        } else {
                            let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                                control: *prop,
                                error: e,
                            })?;
                            set_control_value(handle, *ctrl, val, auto as _)
                        }
                    }
                    AsiControlType::Flip => {
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
                    AsiControlType::Temperature => {
                        let val: f64 =
                            value.try_into().map_err(|e| GenCamError::PropertyError {
                                control: *prop,
                                error: e,
                            })?;
                        let val = (val * 10.0f64) as _;
                        set_control_value(handle, *ctrl, val, auto as _)
                    }
                    AsiControlType::AutoExpMax
                    | AsiControlType::AutoExpTarget
                    | AsiControlType::AutoExpMaxGain
                    | AsiControlType::HighSpeedMode
                    | AsiControlType::CoolerPowerPercent
                    | AsiControlType::TargetTemp => {
                        let val = value.try_into().map_err(|e| GenCamError::PropertyError {
                            control: *prop,
                            error: e,
                        })?;
                        set_control_value(handle, *ctrl, val, auto as _)
                    }
                    _ => Err(GenCamError::PropertyError {
                        control: *prop,
                        error: PropertyError::NotFound,
                    }),
                }
            }
        } else {
            Err(GenCamError::PropertyError {
                control: *prop,
                error: PropertyError::NotFound,
            })
        }
    }

    fn get_flip(&self) -> Result<(bool, bool), GenCamError> {
        let handle = self.handle;
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
        let handle = self.handle;
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
            Ok(self.get_state_raw()? == AsiExposureStatus::Success)
        }
    }
}
