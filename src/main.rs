use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, sleep},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cameraunit_asi::{
    num_cameras, open_first_camera, ASIImageFormat, CameraInfo, CameraUnit, DynamicSerialImage,
    Error, OptimumExposureBuilder, ROI,
};
use chrono::{DateTime, Local};
use configparser::ini::Ini;

#[derive(Debug)]
struct ASICamconfig {
    progname: String,
    savedir: String,
    cadence: Duration,
    max_exposure: Duration,
    percentile: f64,
    max_bin: i32,
    target_val: f32,
    target_uncertainty: f32,
    gain: i32,
    target_temp: f32,
}

fn get_out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap_or("./".to_owned()))
}

fn main() {
    let cfg = ASICamconfig::from_ini(&get_out_dir().join("asicam.ini")).unwrap_or_else(|_| {
        println!(
            "Error reading config file {:#?}, using defaults",
            &get_out_dir().join("asicam.ini").as_os_str()
        );
        let cfg = ASICamconfig::default();
        cfg.to_ini(&get_out_dir().join("asicam.ini")).unwrap();
        cfg
    });
    let num_cameras = num_cameras();
    println!("Found {} cameras", num_cameras);
    if num_cameras <= 0 {
        return;
    }

    let done = Arc::new(AtomicBool::new(false));
    let done_thr = done.clone();
    let done_hdl = done.clone();

    let (mut cam, caminfo) = open_first_camera().unwrap();
    let props = cam.get_props();
    println!("{}", props);

    println!("Setting target temperature: {} C", cfg.target_temp);
    cam.set_temperature(cfg.target_temp).unwrap();

    let cam_ctrlc = caminfo.clone();
    ctrlc::set_handler(move || {
        done_hdl.store(true, Ordering::SeqCst);
        cam_ctrlc.cancel_capture().unwrap_or(()); // This is NOT dropped!!!
        cam_ctrlc.set_cooler(false).unwrap_or(()); // Workaround!
        println!("\nCtrl + C received!");
    })
    .expect("Error setting Ctrl-C handler");

    let camthread = thread::spawn(move || {
        while !done_thr.load(Ordering::SeqCst) {
            // let caminfo = cam;
            sleep(Duration::from_secs(1));
            let temp = caminfo.get_temperature().unwrap();
            let dtime: DateTime<Local> = SystemTime::now().into();
            // let stdout = io::stdout();
            // let _ = write!(&mut stdout.lock(),
            print!(
                "[{}] Camera temperature: {:>+05.1} C, Cooler Power: {:>3}%\t",
                dtime.format("%H:%M:%S"),
                temp,
                caminfo.get_cooler_power().unwrap()
            );
            io::stdout().flush().unwrap();
            print!("\r");
        }
        println!("\nExiting housekeeping thread");
    });
    cam.set_gain_raw(cfg.gain as i64).unwrap();
    cam.set_roi(&ROI {
        x_min: 300,
        y_min: 800,
        width: 2400,
        height: 1300,
        bin_x: 1,
        bin_y: 1,
    })
    .unwrap();
    cam.set_image_fmt(ASIImageFormat::ImageRAW16).unwrap();
    cam.set_exposure(Duration::from_millis(100)).unwrap();
    let exp_ctrl = OptimumExposureBuilder::default()
        .percentile_pix((cfg.percentile * 0.01) as f32)
        .pixel_tgt(cfg.target_val)
        .pixel_uncertainty(cfg.target_uncertainty)
        .pixel_exclusion(100)
        .min_allowed_exp(cam.get_min_exposure().unwrap_or(Duration::from_millis(1)))
        .max_allowed_exp(cfg.max_exposure)
        .max_allowed_bin(cfg.max_bin as u16)
        .build()
        .unwrap();
    'main_loop: while !done.load(Ordering::SeqCst) {
        let mut img: DynamicSerialImage;
        let exp_start: DateTime<Local> = SystemTime::now().into();
        let res = cam.capture_image();
        match res {
            Ok(im) => img = im,
            Err(err) => match err {
                Error::CameraClosed => {
                    done.store(true, Ordering::SeqCst);
                    break 'main_loop;
                }
                Error::CameraRemoved => {
                    done.store(true, Ordering::SeqCst);
                    break 'main_loop;
                }
                Error::InvalidId(_) => {
                    done.store(true, Ordering::SeqCst);
                    break 'main_loop;
                }
                Error::ExposureFailed(msg) => {
                    println!("Exposure failed: {}", msg);
                    continue 'main_loop;
                }
                _ => {
                    continue 'main_loop;
                }
            },
        }
        let mut metadata = img.get_metadata().unwrap(); // CameraUnit_ASI guarantees this
        metadata.add_extended_attrib(
            "exposure_end",
            &format!(
                "{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64()
            ),
        );
        img.set_metadata(metadata);
        let dir_prefix = Path::new(&cfg.savedir).join(exp_start.format("%Y%m%d").to_string());
        if !dir_prefix.exists() {
            std::fs::create_dir_all(&dir_prefix).unwrap();
        }
        let res = img.savefits(&dir_prefix, "comic", Some(&cfg.progname), true, true);
        if let Err(res) = res {
            let res = match res {
                fitsio::errors::Error::ExistingFile(res) => res,
                fitsio::errors::Error::Fits(_) => "Fits Error".to_string(),
                fitsio::errors::Error::Index(_) => "Index error".to_string(),
                fitsio::errors::Error::IntoString(_) => "Into string".to_string(),
                fitsio::errors::Error::Io(_) => "IO Error".to_string(),
                fitsio::errors::Error::Message(res) => res,
                fitsio::errors::Error::Null(_) => "NULL Error".to_string(),
                fitsio::errors::Error::NullPointer => "Nullptr".to_string(),
                fitsio::errors::Error::UnlockError => "Unlock error".to_string(),
                fitsio::errors::Error::Utf8(_) => "UTF-8 error".to_string(),
            };

            println!(
                "\n[{}] AERO: Error saving image: {:#?}",
                exp_start.format("%H:%M:%S"),
                res
            );
        } else {
            println!(
                "\n[{}] AERO: Saved image, exposure {:.3} s",
                exp_start.format("%H:%M:%S"),
                cam.get_exposure().as_secs_f32()
            );
        }
        let (exposure, _bin) = exp_ctrl
            .calculate(
                img.into_luma().into_vec(),
                img.get_metadata().unwrap().exposure,
                img.get_metadata().unwrap().bin_x as u8,
            )
            .unwrap_or((Duration::from_millis(100), 1));
        if exposure != cam.get_exposure() {
            println!(
                "\n[{}] AERO: Exposure changed from {:.3} s to {:.3} s",
                exp_start.format("%H:%M:%S"),
                cam.get_exposure().as_secs_f32(),
                exposure.as_secs_f32()
            );
            cam.set_exposure(exposure).unwrap();
        }
        let val: SystemTime = exp_start.into();
        if val < SystemTime::now() && !done.load(Ordering::SeqCst) {
            sleep(SystemTime::now().duration_since(val).unwrap());
        }
    }
    camthread.join().unwrap();
    println!("\nExiting");
}

impl Default for ASICamconfig {
    fn default() -> Self {
        Self {
            progname: "ASICam".to_string(),
            savedir: "./data".to_string(),
            cadence: Duration::from_secs(20),
            max_exposure: Duration::from_secs(120),
            percentile: 95.0,
            max_bin: 4,
            target_val: 30000.0 / 65536.0,
            target_uncertainty: 2000.0 / 65536.0,
            gain: 100,
            target_temp: -10.0,
        }
    }
}

impl ASICamconfig {
    fn from_ini(path: &PathBuf) -> Result<ASICamconfig, String> {
        let config = Ini::new().load(path)?;
        let mut cfg = ASICamconfig::default();
        if config.contains_key("program") && config["program"].contains_key("name") {
            cfg.progname = config["program"]["name"].clone().unwrap();
        }
        if !config.contains_key("config") {
            return Err("No config section found".to_string());
        }
        if config["config"].contains_key("savedir") {
            cfg.savedir = config["config"]["savedir"].clone().unwrap();
        }
        if config["config"].contains_key("cadence") {
            cfg.cadence = Duration::from_secs(
                config["config"]["cadence"]
                    .clone()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
        }
        if config["config"].contains_key("max_exposure") {
            cfg.max_exposure = Duration::from_secs(
                config["config"]["max_exposure"]
                    .clone()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
        }
        if config["config"].contains_key("percentile") {
            cfg.percentile = config["config"]["percentile"]
                .clone()
                .unwrap()
                .parse::<f64>()
                .unwrap();
        }
        if config["config"].contains_key("maxbin") {
            cfg.max_bin = config["config"]["maxbin"]
                .clone()
                .unwrap()
                .parse::<i32>()
                .unwrap();
        }
        if config["config"].contains_key("value") {
            cfg.target_val = config["config"]["value"]
                .clone()
                .unwrap()
                .parse::<f32>()
                .unwrap();
            cfg.target_val /= 65536.0;
        }
        if config["config"].contains_key("uncertainty") {
            cfg.target_uncertainty = config["config"]["uncertainty"]
                .clone()
                .unwrap()
                .parse::<f32>()
                .unwrap();
            cfg.target_uncertainty /= 65536.0;
        }
        if config["config"].contains_key("gain") {
            cfg.gain = config["config"]["gain"]
                .clone()
                .unwrap()
                .parse::<i32>()
                .unwrap();
        }
        if config["config"].contains_key("target_temp") {
            cfg.target_temp = config["config"]["target_temp"]
                .clone()
                .unwrap()
                .parse::<f32>()
                .unwrap();
        }
        Ok(cfg)
    }

    fn to_ini(&self, path: &PathBuf) -> Result<(), String> {
        let mut config = Ini::new();
        config.set("program", "name", Some(self.progname.clone()));
        config.set("config", "savedir", Some(self.savedir.clone()));
        config.set(
            "config",
            "cadence",
            Some(self.cadence.as_secs().to_string()),
        );
        config.set(
            "config",
            "max_exposure",
            Some(self.max_exposure.as_secs().to_string()),
        );
        config.set("config", "percentile", Some(self.percentile.to_string()));
        config.set("config", "maxbin", Some(self.max_bin.to_string()));
        config.set(
            "config",
            "value",
            Some((self.target_val * 65536.0).to_string()),
        );
        config.set(
            "config",
            "uncertainty",
            Some((self.target_uncertainty * 65536.0).to_string()),
        );
        config.set("config", "gain", Some(self.gain.to_string()));
        config.set("config", "target_temp", Some(self.target_temp.to_string()));
        config.set(
            "config",
            "max_exposure",
            Some(self.max_exposure.as_secs().to_string()),
        );
        config.write(path).map_err(|err| err.to_string())?;
        Ok(())
    }
}
