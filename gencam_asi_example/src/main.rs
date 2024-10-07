use std::{
    env,
    io::{self, Write},
    net::TcpListener,
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
use gencam_packet::{GenCamPacket, PacketType};
use generic_camera_asi::{
    controls::DeviceCtrl, controls::ExposureCtrl, GenCamCtrl, GenCamDriver, GenCamDriverAsi,
    GenCamError, PropertyValue,
};
use refimage::{
    CalcOptExp, ColorSpace, Debayer, DemosaicMethod, DynamicImage, DynamicImageOwned,
    FitsCompression, FitsWrite, GenericImage, GenericImageOwned, ImageProps,
    OptimumExposureBuilder, ToLuma,
};

use image::imageops::FilterType;

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
    save_fits: bool,
    save_png: bool,
}

fn get_out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap_or("./".to_owned()))
}

fn main() {
    // Set up websocket listening.
    let bind_addr = "127.0.0.1:9001";
    let server = TcpListener::bind(bind_addr).unwrap();
    let mut websockets = Vec::with_capacity(10);
    eprintln!("Listening on: ws://{bind_addr}");

    let cfg = ASICamconfig::from_ini(&get_out_dir().join("asicam.ini")).unwrap_or_else(|_| {
        println!(
            "Error reading config file {:#?}, using defaults",
            &get_out_dir().join("asicam.ini").as_os_str()
        );
        let cfg = ASICamconfig::default();
        cfg.to_ini(&get_out_dir().join("asicam.ini")).unwrap();
        cfg
    });
    let mut logfile = std::fs::File::open(&get_out_dir().join("asicam.log"))
        .unwrap_or_else(|_| std::fs::File::create(&get_out_dir().join("asicam.log")).unwrap());
    let mut drv = GenCamDriverAsi;
    let num_cameras = drv.available_devices();
    println!("Found {} cameras", num_cameras);
    if num_cameras == 0 {
        return;
    }

    let done = Arc::new(AtomicBool::new(false));
    let done_thr = done.clone();
    let done_hdl = done.clone();

    let mut cam = drv
        .connect_first_device()
        .expect("Error connecting to camera");
    let info = cam.info().expect("Error getting camera info");
    println!("{:?}", info);

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

    let mut cam_ctrlc = cam.info_handle().expect("Error getting camera handle");

    ctrlc::set_handler(move || {
        done_hdl.store(true, Ordering::SeqCst);
        cam_ctrlc.cancel_capture().unwrap_or(()); // This is NOT dropped!!!
        cam_ctrlc
            .set_property(
                GenCamCtrl::Device(DeviceCtrl::CoolerTemp),
                &PropertyValue::Int(20_i64),
                false,
            )
            .unwrap_or(()); // Set cooler to 20 C
        println!("\nCtrl + C received!");
    })
    .expect("Error setting Ctrl-C handler");

    let caminfo = cam.info_handle().expect("Error getting camera handle");

    let camthread = thread::spawn(move || {
        while !done_thr.load(Ordering::SeqCst) {
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
        println!("\nExiting housekeeping thread");
    });

    cam.set_property(
        GenCamCtrl::Exposure(ExposureCtrl::ExposureTime),
        &(Duration::from_millis(100).into()),
        false,
    )
    .expect("Error setting exposure time");
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
    while !done.load(Ordering::SeqCst) {
        let _roi = cam.get_roi();
        // println!(
        //     "ROI: {}x{} @ {}x{}",
        //     roi.width, roi.height, roi.x_min, roi.y_min
        // );
        let exp_start = Local::now();
        let estart = Instant::now();
        let img = {
            let img = cam.capture();
            match img {
                Ok(img) => img,
                Err(e) => {
                    if e == GenCamError::TimedOut {
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
                            println!("Could not write to log file");
                        }
                        println!("\n[{}] AERO: Timeout", exp_start.format("%H:%M:%S"));
                        continue;
                    } else {
                        panic!("Error capturing image: {:?}", e);
                    }
                }
            }
        };
        let mut img: GenericImage = img.into();
        let save = if last_saved.is_none() {
            true
        } else {
            let elapsed = estart.duration_since(last_saved.unwrap());
            elapsed > cfg.cadence
        };
        if let Some(exp) = img.get_exposure() {
            // save the raw FITS image
            if save {
                last_saved = Some(estart);
                let dir_prefix =
                    Path::new(&cfg.savedir).join(exp_start.format("%Y%m%d").to_string());
                if !dir_prefix.exists() {
                    std::fs::create_dir_all(&dir_prefix).unwrap_or_else(|e| {
                        panic!("Creating directory {:#?}: Error {e:?}", dir_prefix)
                    });
                }
                if cfg.save_fits {
                    let fitsfile = dir_prefix.join(exp_start.format("%H%M%S%.3f.fits").to_string());
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
            if let ColorSpace::Bayer(_) = img.color_space() {
                let dimg: GenericImageOwned = img.clone().into();
                img = dimg
                    .debayer(DemosaicMethod::Nearest)
                    .expect("Error debayering image")
                    .into();
            }

            // Connect to all incoming clients.
            for stream in server.incoming() {
                let websocket = tungstenite::accept(stream.unwrap()).unwrap();
                websockets.push(websocket);
                eprintln!("New client connected!");
            }
            let dimg = DynamicImage::try_from(img.clone()).expect("Error converting image");
            let dimg = dimg.resize(1024, 1024, FilterType::Nearest);

            // Transmit the debayered image to all connected client if transmitting.
            {
                let dimg = dimg.clone();
                let img = DynamicImageOwned::try_from(dimg).expect("Could not convert image.");
                for websocket in &mut websockets {
                    // Converts the DynamicImage to DynamicImageOwned.
                    // Create a new GenCamPacket with the image data.
                    let pkt = GenCamPacket::new(
                        PacketType::Image,
                        0,
                        1024,
                        1024,
                        Some(img.as_raw_u8().to_vec()),
                    );
                    // Set msg to the serialized pkt.
                    let msg = serde_json::to_vec(&pkt).unwrap();
                    // Send the message.
                    websocket.send(msg.into()).unwrap();
                }
            }

            // save the debayerd image as PNG if saving
            if save && cfg.save_png {
                let dir_prefix =
                    Path::new(&cfg.savedir).join(exp_start.format("%Y%m%d").to_string());
                if !dir_prefix.exists() {
                    std::fs::create_dir_all(&dir_prefix).unwrap();
                }
                dimg.save(dir_prefix.join(exp_start.format("%H%M%S%.3f.png").to_string()))
                    .expect("Error saving image");
            }
            // calculate the optimal exposure
            let dimg = img.to_luma().expect("Could not calculate luminance value");
            let (opt_exp, _) = dimg
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
    println!("\nExiting");
}

impl Default for ASICamconfig {
    fn default() -> Self {
        Self {
            progname: "ASICam".to_string(),
            savedir: "./data".to_string(),
            cadence: Duration::from_secs(10),
            max_exposure: Duration::from_secs(120),
            percentile: 95.0,
            max_bin: 4,
            target_val: 30000.0 / 65536.0,
            target_uncertainty: 2000.0 / 65536.0,
            gain: 100,
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
        config.set("config", "save_fits", Some(self.save_fits.to_string()));
        config.set("config", "save_png", Some(self.save_png.to_string()));
        config.write(path).map_err(|err| err.to_string())?;
        Ok(())
    }
}
