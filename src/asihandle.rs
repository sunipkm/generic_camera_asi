#![allow(unused)]
use core::{panic, str};
use std::{
    collections::HashMap,
    ffi::{c_long, CStr},
    fmt::{self, Display, Formatter},
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::sleep,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    zwo_ffi::{
        ASICloseCamera, ASIGetCameraProperty, ASIGetCameraPropertyByID, ASIGetControlCaps,
        ASIGetControlValue, ASIGetDataAfterExp, ASIGetExpStatus, ASIGetNumOfConnectedCameras,
        ASIGetNumOfControls, ASIGetSerialNumber, ASIInitCamera, ASIOpenCamera, ASISetControlValue,
        ASIStartExposure, ASIStopExposure, ASI_BAYER_PATTERN_ASI_BAYER_BG,
        ASI_BAYER_PATTERN_ASI_BAYER_GB, ASI_BAYER_PATTERN_ASI_BAYER_GR,
        ASI_BAYER_PATTERN_ASI_BAYER_RG, ASI_BOOL_ASI_FALSE, ASI_BOOL_ASI_TRUE, ASI_CAMERA_INFO,
        ASI_CONTROL_CAPS, ASI_CONTROL_TYPE_ASI_COOLER_ON, ASI_ID, ASI_IMG_TYPE,
        ASI_IMG_TYPE_ASI_IMG_END, ASI_IMG_TYPE_ASI_IMG_RAW16, ASI_IMG_TYPE_ASI_IMG_RAW8,
    },
    zwo_ffi_wrapper::{
        get_bins, get_control_value, get_pixfmt, map_control_cap, set_control_value,
        string_from_char, to_asibool, AsiControlType, AsiError, AsiExposureStatus, AsiRoi,
    },
    ASICALL,
};

use generic_camera::{
    AnalogCtrl, DeviceCtrl, DigitalIoCtrl, ExposureCtrl, GenCam, GenCamCtrl, SensorCtrl,
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
            .map(|&x| format!("{:02X}", x))
            .collect::<String>();
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
        .map(|&x| format!("{:02X}", x))
        .collect::<String>();
    let mut out = [0u8; 16];
    out.copy_from_slice(sn.as_bytes());
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LastExposureInfo {
    pub roi: AsiRoi,
    pub start: Instant,
    pub tstamp: SystemTime,
    pub exposure: Duration,
    pub darkframe: bool,
    pub gain: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct AsiHandle {
    handle: i32,
    serial: [u8; 16],
    name: [u8; 20],
    bayer: Option<[u8; 4]>, // Bayer pattern
    has_cooler: bool,
    shutter_open: Option<Arc<AtomicBool>>,
    capturing: Arc<AtomicBool>,
    exposure: Arc<AtomicU64>,
    info: Arc<Box<ASI_CAMERA_INFO>>,
    caps: Arc<Box<HashMap<GenCamCtrl, Property>>>,
    icap: Arc<Mutex<CaptureInfo>>,
    pub imgstor: Vec<u8>,
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
        {
            if let Some(e) = ASICALL!(ASIGetControlCaps(handle, i, &mut cap)).err() {
                match e {
                    AsiError::CameraClosed(_, _) => return Err(GenCamError::CameraClosed),
                    AsiError::InvalidId(_, _) => return Err(GenCamError::InvalidId(handle)),
                    _ => continue,
                }
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
        let mut caps: HashMap<GenCamCtrl, Property> =
            caps.iter().filter_map(map_control_cap).collect();
        caps.insert(
            SensorCtrl::PixelFormat.into(),
            Property::new(
                PropertyLims::PixelFmt {
                    variants: get_pixfmt(&info.SupportedVideoFormat, ASI_IMG_TYPE_ASI_IMG_END as _),
                    default: get_pixfmt(&info.SupportedVideoFormat, ASI_IMG_TYPE_ASI_IMG_END as _)
                        [0], // Safety: get_pixfmt() returns at least one element
                },
                false,
                false,
            ),
        );
        if info.IsUSB3Camera == ASI_BOOL_ASI_TRUE as _ {
            caps.insert(
                DeviceCtrl::Custom("UUID".into()).into(),
                Property::new(
                    PropertyLims::EnumStr {
                        variants: Vec::new(),
                        default: "".into(),
                    },
                    false,
                    false,
                ),
            );
        }
        if info.MechanicalShutter == ASI_BOOL_ASI_TRUE as _ {
            let mut prop = Property::new(PropertyLims::Bool { default: true }, false, false);
            prop.set_doc(
                "True if the shutter is open, false if the shutter is closed. Setting this property will open or close the shutter."
            );
            caps.insert(SensorCtrl::ShutterMode.into(), prop);
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
            caps: Arc::new(Box::new(caps)),
            exposure: Arc::new(AtomicU64::new(0)),
            icap: Arc::new(Mutex::new(CaptureInfo {
                roi: roi.clone(),
                last_exposure: None,
            })),
            info: Arc::new(Box::new(info)),
            imgstor: Vec::with_capacity((info.MaxHeight * info.MaxWidth * 2) as _),
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
    pub(crate) fn get_exposure(&self) -> Result<(), GenCamError> {
        let handle = self.handle;
        let (exposure, _) = get_control_value(handle, AsiControlType::Exposure)?;
        self.exposure.store(exposure as _, Ordering::SeqCst);
        Ok(())
    }

    /// Get ROI from device and update internal state
    pub(crate) fn get_roi(&self) -> Result<AsiRoi, GenCamError> {
        let res = AsiRoi::get(self.handle).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        self.icap.lock().unwrap().roi = res.clone();
        Ok(res)
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
        self.icap.lock().unwrap().roi = roi.clone();
        Ok(())
    }

    pub(crate) fn get_gain(&self) -> Result<i64, GenCamError> {
        let handle = self.handle;
        let (gain, _) = get_control_value(handle, AsiControlType::Gain)?;
        Ok(gain)
    }

    pub(crate) fn set_gain(&self, gain: i64) -> Result<(), GenCamError> {
        let handle = self.handle;
        set_control_value(handle, AsiControlType::Gain, gain, ASI_BOOL_ASI_FALSE as _)?;
        Ok(())
    }

    pub(crate) fn get_state(&self) -> Result<GenCamState, GenCamError> {
        let handle = self.handle;
        let capturing = self.capturing.load(Ordering::SeqCst);
        // not currently capturing
        if !capturing {
            return Ok(GenCamState::Idle);
        }
        let mut stat = Default::default();
        ASICALL!(ASIGetExpStatus(self.handle, &mut stat)).map_err(|e| match e {
            AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
            AsiError::InvalidId(_, _) => GenCamError::InvalidId(self.handle),
            _ => GenCamError::GeneralError(format!("{:?}", e)),
        })?;
        let stat: AsiExposureStatus = stat.into();
        match stat {
            // currently capturing, but returned idle?
            AsiExposureStatus::Idle => {
                self.capturing.store(false, Ordering::SeqCst);
                Ok(GenCamState::Errored(GenCamError::ExposureNotStarted))
            }
            // currently capturing
            AsiExposureStatus::Working => {
                if self.icap.lock().unwrap().last_exposure.is_none() {
                    Err(GenCamError::ExposureNotStarted)
                } else {
                    let estart = self.icap.lock().unwrap().last_exposure.unwrap().start;
                    let elapsed = estart.elapsed();
                    Ok(GenCamState::Exposing(Some(elapsed)))
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
        let darkframe = if let Some(open) = (&self.shutter_open) {
            !open.load(Ordering::SeqCst)
        } else {
            false
        };

        if let Ok(mut icap) = self.icap.lock() {
            let roi = icap.roi;
            let mut last_exposure = LastExposureInfo {
                roi: roi.clone(),
                start: Instant::now(),
                tstamp: SystemTime::now(),
                exposure: Duration::from_micros(self.exposure.load(Ordering::SeqCst)),
                darkframe,
                gain: self.get_gain().ok(),
            };
            ASICALL!(ASIStartExposure(handle, to_asibool(darkframe) as _)).map_err(|e| {
                self.capturing.store(false, Ordering::SeqCst);
                match e {
                    AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
                    AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
                    _ => GenCamError::GeneralError(format!("{:?}", e)),
                }
            })?;
            icap.last_exposure = Some(last_exposure);
            Ok(())
        } else {
            panic!("Mutex poisoned");
        }
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
        if let Ok(mut icap) = self.icap.lock() {
            icap.last_exposure = None;
        } else {
            panic!("Mutex poisoned");
        }
        res
    }

    pub fn download_image(&mut self) -> Result<GenericImage, GenCamError> {
        lazy_static::lazy_static! {
            static ref IMGCTR: AtomicU32 = AtomicU32::new(0);
        };
        if !self.capturing.load(Ordering::SeqCst) {
            return Err(GenCamError::ExposureNotStarted);
        }
        let handle = self.handle;
        let state = self.get_state()?;
        let temp = self.get_temperature().unwrap_or(-273.16);
        if let Ok(mut icap) = self.icap.lock() {
            match state {
                GenCamState::Exposing(_) => {
                    return Err(GenCamError::ExposureInProgress);
                }
                GenCamState::Idle => {
                    self.capturing.store(false, Ordering::SeqCst);
                    icap.last_exposure = None;
                    return Err(GenCamError::ExposureNotStarted);
                }
                GenCamState::Errored(e) => {
                    return Err(e);
                }
                GenCamState::Downloading(_) => {
                    return Err(GenCamError::AccessViolation);
                }
                GenCamState::ExposureFinished => {
                    let now = SystemTime::now();
                    let Some(expinfo) = icap.last_exposure.take() else {
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
                    let width = expinfo.roi.width as _;
                    let height = expinfo.roi.height as _;
                    let ptr = &mut self.imgstor;
                    let img: DynamicImageData = match (expinfo.roi.fmt as u32).into() {
                        GenCamPixelBpp::Bpp8 => {
                            let img = ImageData::from_mut_ref(
                                &mut ptr[..(width * height)],
                                width,
                                height,
                                refimage::ColorSpace::Gray,
                            )
                            .map_err(|e| GenCamError::InvalidFormat(format!("{:?}", e)))?;
                            img.into()
                        }
                        GenCamPixelBpp::Bpp16 => {
                            let ptr = bytemuck::try_cast_slice_mut(ptr)
                                .map_err(|e| GenCamError::InvalidFormat(format!("{:?}", e)))?;
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
                                format!("Unexpected pixel format: {:?}", expinfo.roi.fmt)
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
                    img.insert_key("XOFFSET", (expinfo.roi.x, "X offset"));
                    img.insert_key("YOFFSET", (expinfo.roi.y, "Y offset"));
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
                        img.insert_key("XBAYOFF", (expinfo.roi.x % 2, "X offset of Bayer pattern"));
                        img.insert_key("YBAYOFF", (expinfo.roi.y % 2, "Y offset of Bayer pattern"));
                    }
                    return Ok(img);
                }
                _ => {
                    return Err(GenCamError::GeneralError({
                        format!("Unexpected state: {:?}", state)
                    }));
                }
            };
        } else {
            panic!("Mutex poisoned");
        }
    }
}
