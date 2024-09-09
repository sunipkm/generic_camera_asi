#![allow(unused)]
use std::{
    ffi::{c_long, CStr},
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    thread::sleep,
    time::Duration,
};

use crate::zwo_ffi::*;

use generic_camera::{GenCamDescriptor, GenCamError, GenCamPixelBpp, GenCamRoi, GenCamState, Property, PropertyConcrete, PropertyLims};
use generic_camera::{AnalogCtrl, SensorCtrl, DigitalIoCtrl, ExposureCtrl, DeviceCtrl};

use log::warn;

impl Default for ASI_CAMERA_INFO {
    fn default() -> Self {
        Self {
            Name: [0; 64],
            CameraID: Default::default(),
            MaxHeight: Default::default(),
            MaxWidth: Default::default(),
            IsColorCam: Default::default(),
            BayerPattern: Default::default(),
            SupportedBins: Default::default(),
            SupportedVideoFormat: Default::default(),
            PixelSize: Default::default(),
            MechanicalShutter: Default::default(),
            ST4Port: Default::default(),
            IsCoolerCam: Default::default(),
            IsUSB3Host: Default::default(),
            IsUSB3Camera: Default::default(),
            ElecPerADU: Default::default(),
            BitDepth: Default::default(),
            IsTriggerCam: Default::default(),
            Unused: Default::default(),
        }
    }
}

impl Default for ASI_CONTROL_CAPS {
    fn default() -> Self {
        Self {
            Name: [0; 64],
            Description: [0; 128],
            MaxValue: Default::default(),
            MinValue: Default::default(),
            DefaultValue: Default::default(),
            IsAutoSupported: Default::default(),
            ControlType: Default::default(),
            IsWritable: Default::default(),
            Unused: Default::default(),
        }
    }
}

impl From<ASI_CAMERA_INFO> for GenCamDescriptor {
    fn from(value: ASI_CAMERA_INFO) -> Self {
        let name = unsafe { CStr::from_ptr(value.Name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        GenCamDescriptor {
            id: value.CameraID as _,
            name,
            vendor: "ZWO".to_string(),
            model: "ASI".to_string(),
            serial: None,
            description: None,
        }
    }
}

pub(crate) fn get_asi_devs() -> Result<Vec<ASI_CAMERA_INFO>, AsiError> {
    let num_cameras = unsafe { ASIGetNumOfConnectedCameras() };
    let mut devs = Vec::with_capacity(num_cameras as _);
    for id in 0..num_cameras {
        let mut dev = ASI_CAMERA_INFO::default();
        let ret = unsafe { ASIGetCameraProperty(&mut dev, id) };
        if ret != AsiError::Success as _ {
            log::warn!(
                "Failed to get camera property for {id}: {:?}",
                AsiError::from(ret as u32)
            );
            continue;
        }
        devs.push(dev);
    }
    Ok(devs)
}

#[derive(Debug)]
pub(crate) struct AsiHandle {
    handle: i32,
}

impl Drop for AsiHandle {
    fn drop(&mut self) {
        let handle = self.handle;
        let ret = unsafe { ASIStopExposure(handle) };
        if ret != AsiError::Success as _ {
            warn!("Failed to stop exposure: {:?}", AsiError::from(ret as u32));
        }
        // TODO: Turn cooler off
        let ret = unsafe { ASICloseCamera(handle) };
        if ret != AsiError::Success as _ {
            warn!("Failed to close camera: {:?}", AsiError::from(ret as u32));
        }
    }
}

macro_rules! ASICALL {
    ($func:ident($($arg:expr),*)) => {
        {
            let res = unsafe { $func($($arg),*) };
            if res != AsiError::Success as _ {
                log::warn!("Error calling {}(): {:?}", stringify!($func), AsiError::from(res as u32));
                return Err(AsiError::from(res as u32));
            }
        }
    };
}

impl AsiHandle {
    pub(crate) fn new(handle: i32) -> Result<Self, AsiError> {
        ASICALL!(ASIOpenCamera(handle));
        ASICALL!(ASIInitCamera(handle));
        Ok(Self { handle })
    }

    pub(crate) fn get_control_caps(&self) -> Result<Vec<ASI_CONTROL_CAPS>, AsiError> {
        let mut num_ctrl = 0;
        ASICALL!(ASIGetNumOfControls(self.handle, &mut num_ctrl));
        let mut caps = Vec::with_capacity(num_ctrl as _);
        for i in 0..num_ctrl {
            let mut cap = ASI_CONTROL_CAPS::default();
            {
                let res = unsafe { ASIGetControlCaps((self.handle), i, (&mut cap)) };
                if res != AsiError::Success as _ {
                    log::warn!("Error calling {}(): {}", stringify!(ASIGetControlCaps), res);
                    continue;
                }
            };
            caps.push(cap);
        }
        Ok(caps)
    }
}

pub(crate) fn map_control_cap(obj: &ASI_CONTROL_CAPS) -> Option<Property> {
    use ASIControlType::*;
    match obj.ControlType.into() {
        Gain => Some(
            Property::new(AnalogCtrl::Gain,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        Gamma => Some(
            Property::new(AnalogCtrl::Gamma,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        Temperature => Some(
            Property::new(DeviceCtrl::Temperature,
            PropertyLims::Float(PropertyConcrete::new(
                obj.MinValue as f64 / 10.0,
                obj.MaxValue as f64 / 10.0,
                0.1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue as f64 / 10.0,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        AutoExpMax => Some(
            Property::new(ExposureCtrl::ExposureTime,
            PropertyLims::Duration(PropertyConcrete::new(
                Duration::from_micros(obj.MinValue as _),
                Duration::from_micros(obj.MaxValue as _),
                Duration::from_micros(1),
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                Duration::from_micros(obj.DefaultValue as _),
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        AutoExpTarget => Some(
            Property::new(ExposureCtrl::AutoTargetBrightness,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        AutoExpMaxGain => Some(
            Property::new(AnalogCtrl::Gain,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        HighSpeedMode => Some(
            Property::new(DeviceCtrl::HighSpeedMode,
            PropertyLims::Bool(PropertyConcrete::new(
                false,
                false,
                false,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue == ASI_BOOL_ASI_TRUE as _,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        CoolerPowerPercent => Some(
            Property::new(DeviceCtrl::CoolerPower,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        TargetTemp => Some(
            Property::new(DeviceCtrl::CoolerTemp,
            PropertyLims::Int(PropertyConcrete::new(
                obj.MinValue,
                obj.MaxValue,
                1,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
                obj.DefaultValue,
            )),
            obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _
        )),
        _ => None,
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub(crate) enum ASIControlType {
    Gain = ASI_CONTROL_TYPE_ASI_GAIN as _,
    Exposure = ASI_CONTROL_TYPE_ASI_EXPOSURE as _,
    Gamma = ASI_CONTROL_TYPE_ASI_GAMMA as _,
    WhiteBalR = ASI_CONTROL_TYPE_ASI_WB_R as _,
    WhiteBalB = ASI_CONTROL_TYPE_ASI_WB_B as _,
    BWOvld = ASI_CONTROL_TYPE_ASI_BANDWIDTHOVERLOAD as _,
    Overclock = ASI_CONTROL_TYPE_ASI_OVERCLOCK as _,
    Temperature = ASI_CONTROL_TYPE_ASI_TEMPERATURE as _,
    Flip = ASI_CONTROL_TYPE_ASI_FLIP as _,
    AutoExpMax = ASI_CONTROL_TYPE_ASI_AUTO_MAX_EXP as _,
    AutoExpTarget = ASI_CONTROL_TYPE_ASI_AUTO_TARGET_BRIGHTNESS as _,
    AutoExpMaxGain = ASI_CONTROL_TYPE_ASI_AUTO_MAX_GAIN as _,
    HardwareBin = ASI_CONTROL_TYPE_ASI_HARDWARE_BIN as _,
    HighSpeedMode = ASI_CONTROL_TYPE_ASI_HIGH_SPEED_MODE as _,
    CoolerPowerPercent = ASI_CONTROL_TYPE_ASI_COOLER_POWER_PERC as _,
    TargetTemp = ASI_CONTROL_TYPE_ASI_TARGET_TEMP as _,
    CoolerOn = ASI_CONTROL_TYPE_ASI_COOLER_ON as _,
    MonoBin = ASI_CONTROL_TYPE_ASI_MONO_BIN as _,
    FanOn = ASI_CONTROL_TYPE_ASI_FAN_ON as _,
    PatternAdjust = ASI_CONTROL_TYPE_ASI_PATTERN_ADJUST as _,
    AntiDewHeater = ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER as _,
    FanAdjust = ASI_CONTROL_TYPE_ASI_FAN_ADJUST as _,
    PwrLedBrightness = ASI_CONTROL_TYPE_ASI_PWRLED_BRIGNT as _,
    UsbHubRst = ASI_CONTROL_TYPE_ASI_USBHUB_RESET as _,
    GpsSupport = ASI_CONTROL_TYPE_ASI_GPS_SUPPORT as _,
    GpsStartLine = ASI_CONTROL_TYPE_ASI_GPS_START_LINE as _,
    GpsEndLine = ASI_CONTROL_TYPE_ASI_GPS_END_LINE as _,
    RollingInterval = ASI_CONTROL_TYPE_ASI_ROLLING_INTERVAL as _,
    Invalid,
}

impl From<u32> for ASIControlType {
    fn from(val: u32) -> Self {
        match val {
            ASI_CONTROL_TYPE_ASI_GAIN => ASIControlType::Gain,
            ASI_CONTROL_TYPE_ASI_EXPOSURE => ASIControlType::Exposure,
            ASI_CONTROL_TYPE_ASI_GAMMA => ASIControlType::Gamma,
            ASI_CONTROL_TYPE_ASI_WB_R => ASIControlType::WhiteBalR,
            ASI_CONTROL_TYPE_ASI_WB_B => ASIControlType::WhiteBalB,
            ASI_CONTROL_TYPE_ASI_BANDWIDTHOVERLOAD => ASIControlType::BWOvld,
            ASI_CONTROL_TYPE_ASI_OVERCLOCK => ASIControlType::Overclock,
            ASI_CONTROL_TYPE_ASI_TEMPERATURE => ASIControlType::Temperature,
            ASI_CONTROL_TYPE_ASI_FLIP => ASIControlType::Flip,
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_EXP => ASIControlType::AutoExpMax,
            ASI_CONTROL_TYPE_ASI_AUTO_TARGET_BRIGHTNESS => ASIControlType::AutoExpTarget,
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_GAIN => ASIControlType::AutoExpMaxGain,
            ASI_CONTROL_TYPE_ASI_HARDWARE_BIN => ASIControlType::HardwareBin,
            ASI_CONTROL_TYPE_ASI_HIGH_SPEED_MODE => ASIControlType::HighSpeedMode,
            ASI_CONTROL_TYPE_ASI_COOLER_POWER_PERC => ASIControlType::CoolerPowerPercent,
            ASI_CONTROL_TYPE_ASI_TARGET_TEMP => ASIControlType::TargetTemp,
            ASI_CONTROL_TYPE_ASI_COOLER_ON => ASIControlType::CoolerOn,
            ASI_CONTROL_TYPE_ASI_MONO_BIN => ASIControlType::MonoBin,
            ASI_CONTROL_TYPE_ASI_FAN_ON => ASIControlType::FanOn,
            ASI_CONTROL_TYPE_ASI_PATTERN_ADJUST => ASIControlType::PatternAdjust,
            ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER => ASIControlType::AntiDewHeater,
            ASI_CONTROL_TYPE_ASI_FAN_ADJUST => ASIControlType::FanAdjust,
            ASI_CONTROL_TYPE_ASI_PWRLED_BRIGNT => ASIControlType::PwrLedBrightness,
            ASI_CONTROL_TYPE_ASI_USBHUB_RESET => ASIControlType::UsbHubRst,
            ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER => ASIControlType::AntiDewHeater,
            ASI_CONTROL_TYPE_ASI_GPS_SUPPORT => ASIControlType::GpsSupport,
            ASI_CONTROL_TYPE_ASI_GPS_START_LINE => ASIControlType::GpsStartLine,
            ASI_CONTROL_TYPE_ASI_GPS_END_LINE => ASIControlType::GpsEndLine,
            ASI_CONTROL_TYPE_ASI_ROLLING_INTERVAL => ASIControlType::RollingInterval,
            _ => ASIControlType::Invalid,
        }
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub(crate) enum AsiError {
    Success = ASI_ERROR_CODE_ASI_SUCCESS as _,
    InvalidIndex = ASI_ERROR_CODE_ASI_ERROR_INVALID_INDEX as _,
    InvalidId = ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as _,
    InvalidControlType = ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE as _,
    CameraClosed = ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as _,
    CameraRemoved = ASI_ERROR_CODE_ASI_ERROR_CAMERA_REMOVED as _,
    InvalidPath = ASI_ERROR_CODE_ASI_ERROR_INVALID_PATH as _,
    InvalidFileFormat = ASI_ERROR_CODE_ASI_ERROR_INVALID_FILEFORMAT as _,
    InvalidSize = ASI_ERROR_CODE_ASI_ERROR_INVALID_SIZE as _,
    InvalidImage = ASI_ERROR_CODE_ASI_ERROR_INVALID_IMGTYPE as _,
    OutOfBounds = ASI_ERROR_CODE_ASI_ERROR_OUTOF_BOUNDARY as _,
    Timeout = ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as _,
    InvalidSequence = ASI_ERROR_CODE_ASI_ERROR_INVALID_SEQUENCE as _,
    BufferTooSmall = ASI_ERROR_CODE_ASI_ERROR_BUFFER_TOO_SMALL as _,
    VideoModeActive = ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE as _,
    ExposureInProgress = ASI_ERROR_CODE_ASI_ERROR_EXPOSURE_IN_PROGRESS as _,
    GeneralError = ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR as _,
    InvalidMode = ASI_ERROR_CODE_ASI_ERROR_INVALID_MODE as _,
}

impl From<u32> for AsiError {
    fn from(val: u32) -> Self {
        match val {
            ASI_ERROR_CODE_ASI_SUCCESS => AsiError::Success,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_INDEX => AsiError::InvalidIndex,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_ID => AsiError::InvalidId,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE => AsiError::InvalidControlType,
            ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED => AsiError::CameraClosed,
            ASI_ERROR_CODE_ASI_ERROR_CAMERA_REMOVED => AsiError::CameraRemoved,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_PATH => AsiError::InvalidPath,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_FILEFORMAT => AsiError::InvalidFileFormat,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_SIZE => AsiError::InvalidSize,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_IMGTYPE => AsiError::InvalidImage,
            ASI_ERROR_CODE_ASI_ERROR_OUTOF_BOUNDARY => AsiError::OutOfBounds,
            ASI_ERROR_CODE_ASI_ERROR_TIMEOUT => AsiError::Timeout,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_SEQUENCE => AsiError::InvalidSequence,
            ASI_ERROR_CODE_ASI_ERROR_BUFFER_TOO_SMALL => AsiError::BufferTooSmall,
            ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE => AsiError::VideoModeActive,
            ASI_ERROR_CODE_ASI_ERROR_EXPOSURE_IN_PROGRESS => AsiError::ExposureInProgress,
            ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR => AsiError::GeneralError,
            ASI_ERROR_CODE_ASI_ERROR_INVALID_MODE => AsiError::InvalidMode,
            _ => AsiError::GeneralError,
        }
    }
}
