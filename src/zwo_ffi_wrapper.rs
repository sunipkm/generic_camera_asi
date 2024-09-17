use std::{collections::HashMap, fmt::Display, os::raw, time::Duration};

use generic_camera::{
    AnalogCtrl, DeviceCtrl, ExposureCtrl, GenCamCtrl, GenCamDescriptor, GenCamError,
    GenCamPixelBpp, Property, PropertyLims,
};

use crate::zwo_ffi::*;

#[macro_export]
/// Generate a closure that wraps an ASI function call that returns
/// [`Result<(), AsiError>`].
macro_rules! ASICALL {
    ($func:ident($($arg:expr),*)) => {
        (|| -> Result<(), $crate::zwo_ffi_wrapper::AsiError> {
            #[allow(clippy::macro_metavars_in_unsafe)]
            let res = unsafe { $func($($arg),*) };
            if res != $crate::zwo_ffi::ASI_ERROR_CODE_ASI_SUCCESS as _ {
                #[cfg(debug_assertions)]
                let err = {
                    let args = vec![$(stringify!($arg)),*];
                    let args = args.join(", ");
                    let err = $crate::zwo_ffi_wrapper::AsiError::from((res as u32, Some(stringify!($func)), Some(args.as_str())));
                    log::warn!("Error calling {}", err);
                    err
                };
                #[cfg(not(debug_assertions))]
                let err = {
                    $crate::zwo_ffi_wrapper::AsiError::from((res as u32, Some(stringify!($func)), None))
                };
                return Err(err);
            }
            Ok(())
        })()
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

impl Default for ASI_ID {
    fn default() -> Self {
        Self {
            id: Default::default(),
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

pub(crate) fn string_from_char<const N: usize>(inp: &[raw::c_char; N]) -> String {
    let mut str = String::from_utf8_lossy(&unsafe {
        std::mem::transmute_copy::<[raw::c_char; N], [u8; N]>(inp)
    })
    .to_string();
    str.retain(|c| c != '\0');
    str.trim().to_string()
}

impl From<ASI_CAMERA_INFO> for GenCamDescriptor {
    fn from(value: ASI_CAMERA_INFO) -> Self {
        let name = string_from_char(&value.Name);
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
            vendor: "ZWO".into(),
            info,
        }
    }
}

pub fn get_control_value(handle: i32, control: AsiControlType) -> Result<(i64, i32), GenCamError> {
    let mut value = Default::default();
    let mut auto = Default::default();
    let handle = handle as _;
    let control = control as _;
    ASICALL!(ASIGetControlValue(
        handle,
        control,
        &mut value as _,
        &mut auto as _
    ))
    .map_err(|e| match e {
        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        AsiError::InvalidControlType(_, _) => GenCamError::InvalidControlType(control.to_string()),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })?;
    Ok((value, auto))
}

pub fn set_control_value(
    handle: i32,
    control: AsiControlType,
    value: i64,
    auto: ASI_BOOL,
) -> Result<(), GenCamError> {
    let handle = handle as _;
    let control = control as _;
    let value = value as _;
    let auto = auto as _;
    ASICALL!(ASISetControlValue(handle, control, value, auto)).map_err(|e| match e {
        AsiError::CameraClosed(_, _) => GenCamError::CameraClosed,
        AsiError::InvalidId(_, _) => GenCamError::InvalidId(handle),
        AsiError::InvalidControlType(_, _) => GenCamError::InvalidControlType(control.to_string()),
        _ => GenCamError::GeneralError(format!("{:?}", e)),
    })
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

pub(crate) fn map_control_cap(
    obj: &ASI_CONTROL_CAPS,
) -> Option<(GenCamCtrl, (AsiControlType, Property))> {
    use AsiControlType::*;
    match obj.ControlType.into() {
        Gain => Some((
            AnalogCtrl::Gain.into(),
            (
                Gain,
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
            ),
        )),
        Gamma => Some((
            AnalogCtrl::Gamma.into(),
            (
                Gamma,
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
            ),
        )),
        Temperature => Some((
            DeviceCtrl::Temperature.into(),
            (
                Temperature,
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
            ),
        )),
        AutoExpMax => Some((
            ExposureCtrl::ExposureTime.into(),
            (
                AutoExpMax,
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
            ),
        )),
        AutoExpTarget => Some((
            ExposureCtrl::AutoTargetBrightness.into(),
            (
                AutoExpTarget,
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
            ),
        )),
        AutoExpMaxGain => Some((
            AnalogCtrl::Gain.into(),
            (
                AutoExpMaxGain,
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
            ),
        )),
        HighSpeedMode => Some((
            DeviceCtrl::HighSpeedMode.into(),
            (
                HighSpeedMode,
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
            ),
        )),
        CoolerPowerPercent => Some((
            DeviceCtrl::CoolerPower.into(),
            (
                CoolerPowerPercent,
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
            ),
        )),
        TargetTemp => Some((
            DeviceCtrl::CoolerTemp.into(),
            (
                TargetTemp,
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
            ),
        )),
        _ => None,
    }
}

/// ASI Camera ROI
#[derive(Debug, Clone, Copy)]
pub(crate) struct AsiRoi {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub bin: i32,
    pub fmt: ASI_IMG_TYPE,
}

impl AsiRoi {
    /// Get the current ROI
    pub(crate) fn get(handle: i32) -> Result<Self, AsiError> {
        let mut x = 0;
        let mut y = 0;
        let mut width = 0;
        let mut height = 0;
        let mut bin = 0;
        let mut fmt = 0;
        ASICALL!(ASIGetStartPos(handle, &mut x, &mut y))?;
        ASICALL!(ASIGetROIFormat(
            handle,
            &mut width,
            &mut height,
            &mut bin,
            &mut fmt
        ))?;
        Ok(Self {
            x,
            y,
            width,
            height,
            bin,
            fmt: fmt.into(),
        })
    }

    /// Set the ROI
    pub(crate) fn set(&self, handle: i32) -> Result<(), AsiError> {
        ASICALL!(ASISetStartPos(handle, self.x, self.y))?;
        ASICALL!(ASISetROIFormat(
            handle,
            self.width,
            self.height,
            self.bin,
            self.fmt as i32
        ))?;
        Ok(())
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub(crate) enum AsiControlType {
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
    Invalid,
}

impl From<u32> for AsiControlType {
    fn from(val: u32) -> Self {
        match val {
            ASI_CONTROL_TYPE_ASI_GAIN => AsiControlType::Gain,
            ASI_CONTROL_TYPE_ASI_EXPOSURE => AsiControlType::Exposure,
            ASI_CONTROL_TYPE_ASI_GAMMA => AsiControlType::Gamma,
            ASI_CONTROL_TYPE_ASI_WB_R => AsiControlType::WhiteBalR,
            ASI_CONTROL_TYPE_ASI_WB_B => AsiControlType::WhiteBalB,
            ASI_CONTROL_TYPE_ASI_BANDWIDTHOVERLOAD => AsiControlType::BWOvld,
            ASI_CONTROL_TYPE_ASI_OVERCLOCK => AsiControlType::Overclock,
            ASI_CONTROL_TYPE_ASI_TEMPERATURE => AsiControlType::Temperature,
            ASI_CONTROL_TYPE_ASI_FLIP => AsiControlType::Flip,
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_EXP => AsiControlType::AutoExpMax,
            ASI_CONTROL_TYPE_ASI_AUTO_TARGET_BRIGHTNESS => AsiControlType::AutoExpTarget,
            ASI_CONTROL_TYPE_ASI_AUTO_MAX_GAIN => AsiControlType::AutoExpMaxGain,
            ASI_CONTROL_TYPE_ASI_HARDWARE_BIN => AsiControlType::HardwareBin,
            ASI_CONTROL_TYPE_ASI_HIGH_SPEED_MODE => AsiControlType::HighSpeedMode,
            ASI_CONTROL_TYPE_ASI_COOLER_POWER_PERC => AsiControlType::CoolerPowerPercent,
            ASI_CONTROL_TYPE_ASI_TARGET_TEMP => AsiControlType::TargetTemp,
            ASI_CONTROL_TYPE_ASI_COOLER_ON => AsiControlType::CoolerOn,
            ASI_CONTROL_TYPE_ASI_MONO_BIN => AsiControlType::MonoBin,
            ASI_CONTROL_TYPE_ASI_FAN_ON => AsiControlType::FanOn,
            ASI_CONTROL_TYPE_ASI_PATTERN_ADJUST => AsiControlType::PatternAdjust,
            ASI_CONTROL_TYPE_ASI_ANTI_DEW_HEATER => AsiControlType::AntiDewHeater,
            _ => AsiControlType::Invalid,
        }
    }
}

#[repr(i32)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) enum AsiError {
    InvalidIndex(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_INDEX as _,
    InvalidId(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_ID as _,
    InvalidControlType(Option<String>, Option<String>) =
        ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE as _,
    CameraClosed(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED as _,
    CameraRemoved(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_CAMERA_REMOVED as _,
    InvalidPath(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_PATH as _,
    InvalidFileFormat(Option<String>, Option<String>) =
        ASI_ERROR_CODE_ASI_ERROR_INVALID_FILEFORMAT as _,
    InvalidSize(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_SIZE as _,
    InvalidImage(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_IMGTYPE as _,
    OutOfBounds(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_OUTOF_BOUNDARY as _,
    Timeout(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_TIMEOUT as _,
    InvalidSequence(Option<String>, Option<String>) =
        ASI_ERROR_CODE_ASI_ERROR_INVALID_SEQUENCE as _,
    BufferTooSmall(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_BUFFER_TOO_SMALL as _,
    VideoModeActive(Option<String>, Option<String>) =
        ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE as _,
    ExposureInProgress(Option<String>, Option<String>) =
        ASI_ERROR_CODE_ASI_ERROR_EXPOSURE_IN_PROGRESS as _,
    GeneralError(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR as _,
    InvalidMode(Option<String>, Option<String>) = ASI_ERROR_CODE_ASI_ERROR_INVALID_MODE as _,
}

impl<T: Into<String>> From<(u32, Option<T>, Option<T>)> for AsiError {
    fn from(val: (u32, Option<T>, Option<T>)) -> Self {
        let (val, src, args) = val;
        let src = src.map(|x| x.into());
        let args = args.map(|x| x.into());
        match val {
            ASI_ERROR_CODE_ASI_ERROR_INVALID_INDEX => AsiError::InvalidIndex(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_ID => AsiError::InvalidId(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_CONTROL_TYPE => {
                AsiError::InvalidControlType(src, args)
            }
            ASI_ERROR_CODE_ASI_ERROR_CAMERA_CLOSED => AsiError::CameraClosed(src, args),
            ASI_ERROR_CODE_ASI_ERROR_CAMERA_REMOVED => AsiError::CameraRemoved(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_PATH => AsiError::InvalidPath(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_FILEFORMAT => AsiError::InvalidFileFormat(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_SIZE => AsiError::InvalidSize(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_IMGTYPE => AsiError::InvalidImage(src, args),
            ASI_ERROR_CODE_ASI_ERROR_OUTOF_BOUNDARY => AsiError::OutOfBounds(src, args),
            ASI_ERROR_CODE_ASI_ERROR_TIMEOUT => AsiError::Timeout(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_SEQUENCE => AsiError::InvalidSequence(src, args),
            ASI_ERROR_CODE_ASI_ERROR_BUFFER_TOO_SMALL => AsiError::BufferTooSmall(src, args),
            ASI_ERROR_CODE_ASI_ERROR_VIDEO_MODE_ACTIVE => AsiError::VideoModeActive(src, args),
            ASI_ERROR_CODE_ASI_ERROR_EXPOSURE_IN_PROGRESS => {
                AsiError::ExposureInProgress(src, args)
            }
            ASI_ERROR_CODE_ASI_ERROR_GENERAL_ERROR => AsiError::GeneralError(src, args),
            ASI_ERROR_CODE_ASI_ERROR_INVALID_MODE => AsiError::InvalidMode(src, args),
            _ => AsiError::GeneralError(src, args),
        }
    }
}

impl Display for AsiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use AsiError::*;
        let (err, caller, args) = match self {
            InvalidIndex(src, args) => ("Invalid Index", src, args),
            InvalidId(src, args) => ("Invalid ID", src, args),
            InvalidControlType(src, args) => ("Invalid Control Type", src, args),
            CameraClosed(src, args) => ("Camera Closed", src, args),
            CameraRemoved(src, args) => ("Camera Removed", src, args),
            InvalidPath(src, args) => ("Invalid Path", src, args),
            InvalidFileFormat(src, args) => ("Invalid File Format", src, args),
            InvalidSize(src, args) => ("Invalid Size", src, args),
            InvalidImage(src, args) => ("Invalid Image", src, args),
            OutOfBounds(src, args) => ("Out of Bounds", src, args),
            Timeout(src, args) => ("Timeout", src, args),
            InvalidSequence(src, args) => ("Invalid Sequence", src, args),
            BufferTooSmall(src, args) => ("Buffer Too Small", src, args),
            VideoModeActive(src, args) => ("Video Mode Active", src, args),
            ExposureInProgress(src, args) => ("Exposure In Progress", src, args),
            GeneralError(src, args) => ("General Error", src, args),
            InvalidMode(src, args) => ("Invalid Mode", src, args),
        };
        if let Some(caller) = caller {
            if let Some(args) = args {
                write!(f, "{}({}): {}", caller, args, err)
            } else {
                write!(f, "{}(): {}", caller, err)
            }
        } else {
            write!(f, "Operation: {}", err)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AsiExposureStatus {
    Idle = ASI_EXPOSURE_STATUS_ASI_EXP_IDLE as _,
    Working = ASI_EXPOSURE_STATUS_ASI_EXP_WORKING as _,
    Success = ASI_EXPOSURE_STATUS_ASI_EXP_SUCCESS as _,
    Failed = ASI_EXPOSURE_STATUS_ASI_EXP_FAILED as _,
}

impl From<ASI_EXPOSURE_STATUS> for AsiExposureStatus {
    fn from(val: ASI_EXPOSURE_STATUS) -> Self {
        match val {
            ASI_EXPOSURE_STATUS_ASI_EXP_IDLE => AsiExposureStatus::Idle,
            ASI_EXPOSURE_STATUS_ASI_EXP_WORKING => AsiExposureStatus::Working,
            ASI_EXPOSURE_STATUS_ASI_EXP_SUCCESS => AsiExposureStatus::Success,
            ASI_EXPOSURE_STATUS_ASI_EXP_FAILED => AsiExposureStatus::Failed,
            _ => AsiExposureStatus::Idle,
        }
    }
}

pub(crate) fn to_asibool(v: bool) -> ASI_BOOL {
    if v {
        ASI_BOOL_ASI_TRUE
    } else {
        ASI_BOOL_ASI_FALSE
    }
}
