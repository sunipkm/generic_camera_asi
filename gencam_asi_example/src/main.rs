use std::{
    env,
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Local};
use configparser::ini::Ini;
use generic_camera_asi::{
    controls::{AnalogCtrl, DeviceCtrl, ExposureCtrl, SensorCtrl},
    GenCamCtrl, GenCamDriver, GenCamDriverAsi, GenCamError, GenCamPixelBpp, PropertyValue,
};
#[allow(unused_imports)]
use refimage::{
    CalcOptExp, DemosaicMethod, DynamicImage, FitsCompression, FitsWrite, GenericImage, ImageProps,
    OptimumExposureBuilder, ToLuma,
};

#[cfg(feature = "image")]
use image::imageops::FilterType;
#[cfg(feature = "rppal")]
use rppal::gpio::Gpio;

#[cfg(feature = "rppal")]
const GPIO_PWR: u8 = 26;

#[derive(Debug)]
struct ASICamconfig {
    camera: Option<String>,
    progname: String,
    savedir: String,
    cadence: Duration,
    max_exposure: Duration,
    percentile: f64,
    max_bin: i32,
    target_val: f32,
    target_uncertainty: f32,
    gain: Option<f64>,
    target_temp: f32,
    save_fits: bool,
    save_png: bool,
}

fn get_out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap_or("./".to_owned()))
}

fn main() {
    #[cfg(feature = "rppal")]
    let mut power_pin = {
        println!("Initializing GPIO");
        let mut p = Gpio::new()
            .expect("Error opening GPIO")
            .get(GPIO_PWR)
            .expect(&format!("Could not open pin {GPIO_PWR}"))
            .into_output();
        p.set_high(); // turn on power
        p
    };
    let main_run = Arc::new(AtomicBool::new(true));
    // ctrl + c handler to stop the main loop
    {
        let main_run = main_run.clone();
        ctrlc::set_handler(move || {
            main_run.store(false, Ordering::SeqCst);
            println!("\nCtrl + C received!");
        })
        .expect("Error setting Ctrl-C handler");
    }

    // main loop
    while main_run.load(Ordering::SeqCst) {
        let cfg = ASICamconfig::from_ini(&get_out_dir().join("asicam.ini")).unwrap_or_else(|_| {
            println!(
                "Error reading config file {:#?}, using defaults",
                &get_out_dir().join("asicam.ini").as_os_str()
            );
            let cfg = ASICamconfig::default();
            cfg.to_ini(&get_out_dir().join("asicam.ini")).unwrap();
            cfg
        });
        let mut logfile = OpenOptions::new()
            .create(true)
            .append(true)
            .open(get_out_dir().join("asicam.log"))
            .expect("Error opening log file");
        let mut drv = GenCamDriverAsi;
        let num_cameras = drv.available_devices();
        println!("Found {} cameras", num_cameras);
        if num_cameras == 0 {
            return;
        }

        let sub_run = Arc::new(AtomicBool::new(true));

        let mut cam = {
            if let Some(cam_name) = &cfg.camera {
                println!("Connecting to camera: {}", cam_name);
                let devlist = drv.list_devices().expect("Could not list devices");
                let dev = devlist
                    .iter()
                    .find(|d| d.name.contains(cam_name))
                    .expect("Could not find camera");
                drv.connect_device(dev).expect("Error connecting to camera")
            } else {
                drv.connect_first_device()
                    .expect("Error connecting to camera")
            }
        };
        let info = cam.info().expect("Error getting camera info").clone();
        println!("{:?}", info);

        if let Some(color) = info.info.get("Color Sensor") {
            if let Some(color) = color.as_bool() {
                if !color {
                    println!("Setting pixel format to 16-bit");
                    cam.set_property(
                        SensorCtrl::PixelFormat.into(),
                        &GenCamPixelBpp::Bpp16.into(),
                        false,
                    )
                    .expect("Error setting pixel format");
                }
            }
        }

        println!("Setting target temperature: {} C", cfg.target_temp);
        if cam
            .set_property(
                GenCamCtrl::Device(DeviceCtrl::CoolerTemp),
                &PropertyValue::Int(cfg.target_temp as i64),
                false,
            )
            .is_err()
        {
            println!("Error setting target temperature");
        }

        let caminfo = cam.info_handle().expect("Error getting camera handle");

        let camthread = {
            let main_run = main_run.clone();
            let sub_run = sub_run.clone();
            thread::spawn(move || {
                while sub_run.load(Ordering::SeqCst) && main_run.load(Ordering::SeqCst) {
                    // let caminfo = cam;
                    sleep(Duration::from_secs(1));
                    let (temp, _) = caminfo
                        .get_property(GenCamCtrl::Device(DeviceCtrl::Temperature))
                        .unwrap_or((PropertyValue::from(-273.15), false));
                    let dtime: DateTime<Local> = SystemTime::now().into();
                    // let stdout = io::stdout();
                    // let _ = write!(&mut stdout.lock(),
                    print!(
                        "[{}] Camera temperature: {:>+05.1} C, Cooler Power: {:>3}%\t",
                        dtime.format("%H:%M:%S"),
                        temp.try_into().unwrap_or(-273.15),
                        caminfo
                            .get_property(GenCamCtrl::Device(DeviceCtrl::CoolerPower))
                            .unwrap_or((PropertyValue::from(-1i64), false))
                            .0
                            .try_into()
                            .unwrap_or(-1i64)
                    );
                    io::stdout().flush().unwrap();
                    print!("\r");
                }
                if let Err(e) = caminfo.cancel_capture() {
                    println!("Error cancelling capture: {:#?}", e);
                }
                println!("\nExiting housekeeping thread");
            })
        };

        cam.set_property(
            GenCamCtrl::Exposure(ExposureCtrl::ExposureTime),
            &(Duration::from_millis(100).into()),
            false,
        )
        .expect("Error setting exposure time");
        // gain settings
        cam.list_properties()
            .get(&AnalogCtrl::Gain.into())
            .map(|prop| {
                println!("Gain Settings: {:#?}", prop);
            });
        if let Ok((gain, auto)) = cam.get_property(AnalogCtrl::Gain.into()) {
            println!(
                "Current gain: {:.1} dB, Auto mode: {}",
                gain.as_f64().unwrap_or(-1.0),
                auto
            );
        }
        if let Some(gain) = cfg.gain {
            println!("Setting gain to {:.1} dB", gain);
            cam.set_property(AnalogCtrl::Gain.into(), &gain.into(), false)
                .expect("Error setting gain");
        } else {
            // set optimal gain for the cameras we use
            if info.name.contains("533") {
                if let Err(e) = cam.set_property(AnalogCtrl::Gain.into(), &10.0f64.into(), false) {
                    println!("Error setting camera gain: {e:#?}");
                } else {
                    println!("Setting {} gain to 10 dB", &info.name);
                }
            } else if info.name.contains("432") {
                if let Err(e) = cam.set_property(AnalogCtrl::Gain.into(), &14.0f64.into(), false) {
                    println!("Error setting camera gain: {e:#?}");
                } else {
                    println!("Setting {} gain to 14 dB", &info.name);
                }
            } else if info.name.contains("585") {
                if let Err(e) = cam.set_property(AnalogCtrl::Gain.into(), &25.2f64.into(), false) {
                    println!("Error setting camera gain: {e:#?}");
                } else {
                    println!("Setting {} gain to 25.2 dB", &info.name);
                }
            }
        }

        let props = cam.list_properties();
        let exp_prop = props
            .get(&GenCamCtrl::Exposure(ExposureCtrl::ExposureTime))
            .expect("Error getting exposure property");
        let exp_ctrl = OptimumExposureBuilder::default()
            .percentile_pix((cfg.percentile * 0.01) as f32)
            .pixel_tgt(cfg.target_val)
            .pixel_uncertainty(cfg.target_uncertainty)
            .pixel_exclusion(100)
            .min_allowed_exp(
                exp_prop
                    .get_min()
                    .expect("Property does not contain minimum value")
                    .try_into()
                    .expect("Error getting min exposure"),
            )
            .max_allowed_exp(cfg.max_exposure)
            .max_allowed_bin(cfg.max_bin as u16)
            .build()
            .unwrap();
        let mut last_saved = None;
        'exposure_loop: while main_run.load(Ordering::SeqCst) && sub_run.load(Ordering::SeqCst) {
            let _roi = cam.get_roi();
            // println!(
            //     "ROI: {}x{} @ {}x{}",
            //     roi.width, roi.height, roi.x_min, roi.y_min
            // );
            let exp_start = Local::now();
            let img = {
                let img = cam.capture();
                match img {
                    Ok(img) => img,
                    Err(e) => match e {
                        GenCamError::TimedOut => {
                            if logfile
                                .write(
                                    format!(
                                        "[{}] AERO: Timeout\n",
                                        exp_start.format("%Y-%m-%d %H:%M:%S")
                                    )
                                    .as_bytes(),
                                )
                                .is_err()
                            {
                                println!("\nCould not write to log file");
                            }
                            println!("\n[{}] AERO: Timeout", exp_start.format("%H:%M:%S"));
                            continue;
                        }
                        GenCamError::ExposureNotStarted => {
                            // probably ctrl + c was pressed
                            continue;
                        }
                        GenCamError::ExposureFailed(reason) => {
                            println!("Error capturing image: {}, re-enumerating...", reason);
                            sub_run.store(false, Ordering::SeqCst); // indicate to stop the housekeeping thread
                            #[cfg(feature = "rppal")]
                            {
                                power_pin.set_low(); // turn off power
                                sleep(Duration::from_secs(5));
                                power_pin.set_high(); // turn on power
                                sleep(Duration::from_secs(5));
                            }
                            break 'exposure_loop; // re-initialize the camera
                        }
                        _ => {
                            panic!("Error capturing image: {:?}", e);
                        }
                    },
                }
            };
            let img: GenericImage = img.into();
            let save = match last_saved {
                None => true,
                Some(last_saved) => {
                    let elapsed = Instant::now().duration_since(last_saved);
                    elapsed > cfg.cadence
                }
            };
            if let Some(exp) = img.get_exposure() {
                // save the raw FITS image
                if save {
                    last_saved = Some(Instant::now());
                    let dir_prefix =
                        Path::new(&cfg.savedir).join(exp_start.format("%Y%m%d").to_string());
                    if !dir_prefix.exists() {
                        std::fs::create_dir_all(&dir_prefix).unwrap_or_else(|e| {
                            panic!("Creating directory {:#?}: Error {e:?}", dir_prefix)
                        });
                    }
                    if cfg.save_fits {
                        let fitsfile =
                            dir_prefix.join(exp_start.format("%H%M%S%.3f.fits").to_string());
                        if img
                            .write_fits(&fitsfile, FitsCompression::Rice, true)
                            .is_err()
                        {
                            println!(
                                "\n[{}] AERO: Failed to save FITS image, exposure {:.3} s",
                                exp_start.format("%H:%M:%S"),
                                exp.as_secs_f32()
                            );
                        } else {
                            println!(
                                "\n[{}] AERO: Saved FITS image, exposure {:.3} s",
                                exp_start.format("%H:%M:%S"),
                                exp.as_secs_f32()
                            );
                        }
                    }
                }
                // debayer the image if it is a Bayer image
                let mut img = if img.color_space().is_bayer() {
                    img.debayer(DemosaicMethod::Nearest)
                        .expect("Error debayering image")
                } else {
                    img
                };
                #[cfg(feature = "image")]
                // save the debayerd image as PNG if saving
                if save && cfg.save_png {
                    let dir_prefix =
                        Path::new(&cfg.savedir).join(exp_start.format("%Y%m%d").to_string());
                    if !dir_prefix.exists() {
                        std::fs::create_dir_all(&dir_prefix).unwrap();
                    }
                    let dimg = DynamicImage::try_from(img.clone()).expect("Error converting image");
                    let dimg = dimg.resize(1024, 1024, FilterType::Nearest);

                    dimg.save(dir_prefix.join(exp_start.format("%H%M%S%.3f.png").to_string()))
                        .expect("Error saving image");
                }
                // convert the image to grayscale
                img.to_luma().expect("Error converting image to grayscale");
                // calculate the optimal exposure
                let (opt_exp, _) = img
                    .calc_opt_exp(&exp_ctrl, exp, 1)
                    .expect("Could not calculate optimal exposure");
                if opt_exp != exp {
                    println!(
                        "\n[{}] AERO: Exposure changed from {:.3} s to {:.3} s",
                        exp_start.format("%H:%M:%S"),
                        exp.as_secs_f32(),
                        opt_exp.as_secs_f32()
                    );
                    cam.set_property(
                        GenCamCtrl::Exposure(ExposureCtrl::ExposureTime),
                        &opt_exp.into(),
                        false,
                    )
                    .expect("Error setting exposure time");
                }
            } else {
                println!(
                    "\n[{}] AERO: No exposure value found",
                    exp_start.format("%H:%M:%S")
                );
            }
        }
        camthread.join().unwrap();
    }
    println!("\nExiting");
}

impl Default for ASICamconfig {
    fn default() -> Self {
        Self {
            camera: None, // connect to the first camera
            progname: "ASICam".to_string(),
            savedir: "./data".to_string(),
            cadence: Duration::from_secs(10),
            max_exposure: Duration::from_secs(120),
            percentile: 95.0,
            max_bin: 4,
            target_val: 30000.0 / 65536.0,
            target_uncertainty: 2000.0 / 65536.0,
            gain: None, // use the camera default
            target_temp: -10.0,
            save_fits: false,
            save_png: true,
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
            cfg.max_exposure = Duration::from_secs_f64(
                config["config"]["max_exposure"]
                    .clone()
                    .unwrap()
                    .parse::<f64>()
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
        if config["config"].contains_key("gain") {}
        cfg.gain = config["config"]["gain"]
            .as_ref()
            .map(|v| v.parse::<f64>().ok())
            .flatten();
        if config["config"].contains_key("target_temp") {
            cfg.target_temp = config["config"]["target_temp"]
                .clone()
                .unwrap()
                .parse::<f32>()
                .unwrap();
        }
        if config["config"].contains_key("save_fits") {
            cfg.save_fits = config["config"]["save_fits"]
                .clone()
                .unwrap()
                .parse::<bool>()
                .unwrap();
        } else {
            cfg.save_fits = false;
        }
        if config["config"].contains_key("save_png") {
            cfg.save_png = config["config"]["save_png"]
                .clone()
                .unwrap()
                .parse::<bool>()
                .unwrap();
        } else {
            cfg.save_png = false;
        }
        if config["config"].contains_key("camera") {
            cfg.camera = Some(config["config"]["camera"].clone().unwrap());
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
            Some(format!("{:6}", self.max_exposure.as_secs_f64())),
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
        if let Some(gain) = self.gain {
            config.set("config", "gain", Some(gain.to_string()));
        }
        config.set("config", "target_temp", Some(self.target_temp.to_string()));
        config.set(
            "config",
            "max_exposure",
            Some(self.max_exposure.as_secs().to_string()),
        );
        config.set("config", "save_fits", Some(self.save_fits.to_string()));
        config.set("config", "save_png", Some(self.save_png.to_string()));
        config.write(path).map_err(|err| err.to_string())?;
        Ok(())
    }
}
