use std::{collections::HashMap, ffi::CStr, time::Duration};

use generic_camera::{AnalogCtrl, DeviceCtrl, ExposureCtrl, GenCamCtrl, GenCamDescriptor, GenCamPixelBpp, Property, PropertyLims};

use crate::zwo_ffi::*;

#[macro_export]
macro_rules! ASICALL {
    ($func:ident($($arg:expr),*)) => {
        {
            #[allow(clippy::macro_metavars_in_unsafe)]
            let res = unsafe { $func($($arg),*) };
            if res != $crate::zwo_ffi_wrapper::AsiError::Success as _ {
                log::warn!("Error calling {}(): {:?}", stringify!($func), $crate::zwo_ffi_wrapper::AsiError::from(res as u32));
                return Err($crate::zwo_ffi_wrapper::AsiError::from(res as u32));
            }
        }
    };
}


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
        let mut info = HashMap::new();
        info.insert("Camera ID".to_string(), (value.CameraID as i64).into());
        info.insert("Sensor Height".to_string(), value.MaxHeight.into());
        info.insert("Sensor Width".to_string(), value.MaxWidth.into());
        info.insert(
            "Color Sensor".to_string(),
            (value.IsColorCam == ASI_BOOL_ASI_TRUE as _).into(),
        );
        if value.IsColorCam == ASI_BOOL_ASI_TRUE as _ {
            info.insert(
                "Bayer Pattern".to_string(),
                match value.BayerPattern {
                    ASI_BAYER_PATTERN_ASI_BAYER_BG => "GRBG",
                    ASI_BAYER_PATTERN_ASI_BAYER_GB => "RGGB",
                    ASI_BAYER_PATTERN_ASI_BAYER_GR => "BGGR",
                    ASI_BAYER_PATTERN_ASI_BAYER_RG => "GBRG",
                    _ => "None",
                }
                .to_string()
                .into(),
            );
        }
        info.insert("Pixel Size".to_string(), value.PixelSize.into());
        info.insert(
            "Mechanical Shutter".to_string(),
            (value.MechanicalShutter == ASI_BOOL_ASI_TRUE as _).into(),
        );
        info.insert(
            "ST4 Port".to_string(),
            (value.ST4Port == ASI_BOOL_ASI_TRUE as _).into(),
        );
        info.insert(
            "Cooler".to_string(),
            (value.IsCoolerCam == ASI_BOOL_ASI_TRUE as _).into(),
        );
        info.insert(
            "USB3 Host".to_string(),
            (value.IsUSB3Host == ASI_BOOL_ASI_TRUE as _).into(),
        );
        info.insert(
            "USB3 Device".to_string(),
            (value.IsUSB3Camera == ASI_BOOL_ASI_TRUE as _).into(),
        );
        info.insert(
            "Electrons per ADU".to_string(),
            (value.ElecPerADU as f64).into(),
        );
        info.insert("Bit Depth".to_string(), (value.BitDepth as u64).into());
        info.insert(
            "Trigger".to_string(),
            (value.IsTriggerCam == ASI_BOOL_ASI_TRUE as _).into(),
        );
        GenCamDescriptor {
            id: value.CameraID as _,
            name,
            vendor: "ZWO".to_string(),
            info,
        }
    }
}

pub fn get_pixfmt(list: &[i32], end: i32) -> Vec<GenCamPixelBpp> {
    list.iter()
        .take_while(|x| **x != end)
        .copied()
        .filter_map(|x| match x {
            ASI_IMG_TYPE_ASI_IMG_RAW8 => Some(GenCamPixelBpp::Bpp8),
            ASI_IMG_TYPE_ASI_IMG_RAW16 => Some(GenCamPixelBpp::Bpp16),
            _ => None,
        })
        .collect()
}

pub fn get_bins(list: &[i32], end: i32) -> Vec<u64> {
    list.iter()
        .take_while(|x| **x != end)
        .copied()
        .filter_map(|x| if x > 0 { Some(x as _) } else { None })
        .collect()
}


pub(crate) fn map_control_cap(obj: &ASI_CONTROL_CAPS) -> Option<(GenCamCtrl, Property)> {
    use ASIControlType::*;
    match obj.ControlType.into() {
        Gain => Some((
            AnalogCtrl::Gain.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        Gamma => Some((
            AnalogCtrl::Gamma.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        Temperature => Some((
            DeviceCtrl::Temperature.into(),
            Property::new(
                PropertyLims::Float {
                    min: obj.MinValue as f64 / 10.0,
                    max: obj.MaxValue as f64 / 10.0,
                    step: 0.1,
                    default: obj.DefaultValue as f64 / 10.0,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        AutoExpMax => Some((
            ExposureCtrl::ExposureTime.into(),
            Property::new(
                PropertyLims::Duration {
                    min: Duration::from_micros(obj.MinValue as _),
                    max: Duration::from_micros(obj.MaxValue as _),
                    step: Duration::from_micros(1),
                    default: Duration::from_micros(obj.DefaultValue as _),
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        AutoExpTarget => Some((
            ExposureCtrl::AutoTargetBrightness.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        AutoExpMaxGain => Some((
            AnalogCtrl::Gain.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        HighSpeedMode => Some((
            DeviceCtrl::HighSpeedMode.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        CoolerPowerPercent => Some((
            DeviceCtrl::CoolerPower.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
        )),
        TargetTemp => Some((
            DeviceCtrl::CoolerTemp.into(),
            Property::new(
                PropertyLims::Int {
                    min: obj.MinValue,
                    max: obj.MaxValue,
                    step: 1,
                    default: obj.DefaultValue,
                },
                obj.IsAutoSupported == ASI_BOOL_ASI_TRUE as _,
                obj.IsWritable != ASI_BOOL_ASI_TRUE as _,
            ),
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
