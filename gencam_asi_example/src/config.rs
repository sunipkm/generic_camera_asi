use configparser::ini::Ini;
use std::{path::PathBuf, time::Duration};

#[derive(Debug)]
pub struct ASICamconfig {
    pub camera: Option<String>,
    pub progname: String,
    pub savedir: String,
    pub cadence: Duration,
    pub max_exposure: Duration,
    pub percentile: f64,
    pub max_bin: i32,
    pub target_val: f32,
    pub target_uncertainty: f32,
    pub gain: Option<f64>,
    pub target_temp: f32,
    pub save_fits: bool,
    pub save_png: bool,
    pub pix8b: bool,
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
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
            pix8b: false,
            x_min: 0,
            x_max: 0,
            y_min: 0,
            y_max: 0,
        }
    }
}

impl ASICamconfig {
    pub fn from_ini(path: &PathBuf) -> Result<ASICamconfig, String> {
        let config = Ini::new().load(path)?;
        let mut cfg = ASICamconfig::default();

        // program name
        if let Some(progname) = config.get("program") {
            if let Some(name) = progname.get("name") {
                cfg.progname = name.as_ref().unwrap().clone();
            }
        }

        // config section
        if let Some(config) = config.get("config") {
            if let Some(savedir) = config.get("savedir") {
                cfg.savedir = savedir.as_ref().unwrap().clone();
            }
            if let Some(cadence) = config.get("cadence") {
                cfg.cadence =
                    Duration::from_secs(cadence.as_ref().unwrap().parse::<u64>().unwrap());
            }
            if let Some(max_exposure) = config.get("max_exposure") {
                cfg.max_exposure =
                    Duration::from_secs_f64(max_exposure.as_ref().unwrap().parse::<f64>().unwrap());
            }
            if let Some(percentile) = config.get("percentile") {
                cfg.percentile = percentile.as_ref().unwrap().parse::<f64>().unwrap();
            }
            if let Some(maxbin) = config.get("maxbin") {
                cfg.max_bin = maxbin.as_ref().unwrap().parse::<i32>().unwrap();
            }
            if let Some(value) = config.get("value") {
                cfg.target_val = value.as_ref().unwrap().parse::<f32>().unwrap();
                cfg.target_val /= 65536.0;
            }
            if let Some(uncertainty) = config.get("uncertainty") {
                cfg.target_uncertainty = uncertainty.as_ref().unwrap().parse::<f32>().unwrap();
                cfg.target_uncertainty /= 65536.0;
            }
            if let Some(gain) = config.get("gain") {
                cfg.gain = gain.as_ref().and_then(|v| v.parse::<f64>().ok());
            }
            if let Some(target_temp) = config.get("target_temp") {
                cfg.target_temp = target_temp.as_ref().unwrap().parse::<f32>().unwrap();
            }
            if let Some(save_fits) = config.get("save_fits") {
                cfg.save_fits = save_fits.as_ref().unwrap().parse::<bool>().unwrap();
            } else {
                cfg.save_fits = false;
            }
            if let Some(save_png) = config.get("save_png") {
                cfg.save_png = save_png.as_ref().unwrap().parse::<bool>().unwrap();
            } else {
                cfg.save_png = false;
            }
            if let Some(camera) = config.get("camera") {
                cfg.camera = Some(camera.as_ref().unwrap().clone());
            }
            if let Some(pix8b) = config.get("pix8b") {
                cfg.pix8b = pix8b.as_ref().unwrap().parse::<bool>().unwrap();
            } else {
                cfg.pix8b = false;
            }
            if let Some(x_min) = config.get("x_min") {
                cfg.x_min = x_min.as_ref().unwrap().parse::<i32>().unwrap();
            }
            if let Some(x_max) = config.get("x_max") {
                cfg.x_max = x_max.as_ref().unwrap().parse::<i32>().unwrap();
            }
            if let Some(y_min) = config.get("y_min") {
                cfg.y_min = y_min.as_ref().unwrap().parse::<i32>().unwrap();
            }
            if let Some(y_max) = config.get("y_max") {
                cfg.y_max = y_max.as_ref().unwrap().parse::<i32>().unwrap();
            }
        } else {
            return Err("No config section found".to_string());
        }
        Ok(cfg)
    }

    pub fn to_ini(&self, path: &PathBuf) -> Result<(), String> {
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
        config.set("config", "pix8b", Some(self.pix8b.to_string()));
        config.set("config", "x_min", Some(self.x_min.to_string()));
        config.set("config", "x_max", Some(self.x_max.to_string()));
        config.set("config", "y_min", Some(self.y_min.to_string()));
        config.set("config", "y_max", Some(self.y_max.to_string()));
        config.write(path).map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn change_roi(&self) -> bool {
        self.x_min != 0 || self.x_max != 0 || self.y_min != 0 || self.y_max != 0
    }
}
