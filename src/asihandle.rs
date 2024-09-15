#![allow(unused)]
use std::{
    collections::HashMap,
    ffi::{c_long, CStr},
    fmt::{self, Display, Formatter},
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread::sleep,
    time::{Duration, SystemTime},
};

use crate::{
    zwo_ffi::{
        ASICloseCamera, ASIGetCameraProperty, ASIGetCameraPropertyByID, ASIGetControlCaps,
        ASIGetNumOfConnectedCameras, ASIGetNumOfControls, ASIInitCamera, ASIOpenCamera,
        ASIStopExposure, ASI_CAMERA_INFO, ASI_CONTROL_CAPS, ASI_IMG_TYPE_ASI_IMG_END,
    },
    zwo_ffi_wrapper::{get_bins, get_pixfmt, map_control_cap, AsiError},
    ASICALL,
};

use generic_camera::{AnalogCtrl, DeviceCtrl, DigitalIoCtrl, ExposureCtrl, GenCamCtrl, SensorCtrl};
use generic_camera::{
    GenCamDescriptor, GenCamError, GenCamPixelBpp, GenCamRoi, GenCamState, Property, PropertyLims,
};

use log::warn;

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

// TODO: Check if OpenCamera needs to happen for GetSerialNumber

#[derive(Debug, Clone, Copy)]
pub(crate) struct LastExposureInfo {
    pub roi: GenCamRoi,
    pub bpp: GenCamPixelBpp,
    pub tstamps: SystemTime,
    pub exposure: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct AsiHandle {
    handle: i32,
    last_exposure: Option<LastExposureInfo>,
    capturing: Arc<AtomicBool>,
    caps: HashMap<GenCamCtrl, Property>,
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

fn get_info(handle: i32) -> Result<ASI_CAMERA_INFO, AsiError> {
    let mut info = ASI_CAMERA_INFO::default();
    ASICALL!(ASIGetCameraPropertyByID(handle, &mut info));
    Ok(info)
}

pub(crate) fn get_control_caps(handle: i32) -> Result<Vec<ASI_CONTROL_CAPS>, AsiError> {
    let mut num_ctrl = 0;
    ASICALL!(ASIGetNumOfControls(handle, &mut num_ctrl));
    let mut caps = Vec::with_capacity(num_ctrl as _);
    for i in 0..num_ctrl {
        let mut cap = ASI_CONTROL_CAPS::default();
        {
            let res = unsafe { ASIGetControlCaps((handle), i, (&mut cap)) };
            if res != AsiError::Success as _ {
                log::warn!("Error calling ASIGetControlCaps({handle},{i}): {res}");
                continue;
            }
        };
        caps.push(cap);
    }
    Ok(caps)
}

impl AsiHandle {
    pub(crate) fn new(handle: i32) -> Result<Self, AsiError> {
        ASICALL!(ASIOpenCamera(handle));
        let info = get_info(handle)?;
        let caps = get_control_caps(handle)?;
        let mut caps: HashMap<GenCamCtrl, Property> =
            caps.iter().filter_map(map_control_cap).collect();
        caps.insert(
            SensorCtrl::BinningBoth.into(),
            Property::new(
                PropertyLims::EnumUnsigned {
                    variants: get_bins(&info.SupportedBins, 0 as _),
                    default: 1,
                },
                false,
                false,
            ),
        );
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
        ASICALL!(ASIInitCamera(handle));
        Ok(Self {
            handle,
            last_exposure: None,
            capturing: Arc::new(AtomicBool::new(false)),
            caps,
        })
    }
}
