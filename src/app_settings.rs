use crate::{
    config::{CurvePoint, TargetSelector, normalize_curve_points},
    smc_controller::FanInfo,
};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_refresh_interval_ms")]
    pub refresh_interval_ms: u64,
    #[serde(default)]
    pub fans: Vec<FanSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanSettings {
    pub label: String,
    #[serde(default)]
    pub mode: FanControlMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FanControlMode {
    Adaptive {
        target: TargetSelector,
        max_temp_c: f64,
        hysteresis_c: f64,
    },
    Auto,
    Max,
    Fixed {
        rpm: u32,
    },
    Curve {
        target: TargetSelector,
        hysteresis_c: f64,
        points: Vec<CurvePoint>,
    },
}

impl Default for FanControlMode {
    fn default() -> Self {
        Self::Adaptive {
            target: default_target_selector(),
            max_temp_c: default_max_temp_c(),
            hysteresis_c: default_hysteresis_c(),
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            refresh_interval_ms: default_refresh_interval_ms(),
            fans: Vec::new(),
        }
    }
}

impl AppSettings {
    pub fn settings_path() -> Result<PathBuf> {
        let project_dirs = ProjectDirs::from("com", "denisqsound", "apple-silicon-fan-control")
            .context("failed to resolve Application Support directory")?;
        Ok(project_dirs.config_dir().join("settings.yaml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read settings file `{}`", path.display()))?;
        let mut settings: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse settings file `{}`", path.display()))?;
        settings.normalize()?;
        Ok(settings)
    }

    pub fn load_or_default(path: &Path, fan_infos: &[FanInfo]) -> Result<Self> {
        let mut settings = match path.exists() {
            true => Self::load(path)?,
            false => Self::default(),
        };
        settings.sync_with_fans(fan_infos);
        Ok(settings)
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.normalize()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create settings directory `{}`", parent.display())
            })?;
        }
        let yaml = serde_yaml::to_string(self).context("failed to serialize settings")?;
        fs::write(path, yaml)
            .with_context(|| format!("failed to write settings file `{}`", path.display()))?;
        Ok(())
    }

    pub fn sync_with_fans(&mut self, fan_infos: &[FanInfo]) {
        if self.fans.len() > fan_infos.len() {
            self.fans.truncate(fan_infos.len());
        }

        for (index, fan_info) in fan_infos.iter().enumerate() {
            if let Some(existing) = self.fans.get_mut(index) {
                if existing.label.trim().is_empty() {
                    existing.label = default_fan_label(index);
                }
                if !matches!(
                    existing.mode,
                    FanControlMode::Adaptive { .. } | FanControlMode::Auto | FanControlMode::Max
                ) {
                    existing.set_adaptive_default(fan_info);
                }
                continue;
            }

            self.fans.push(FanSettings {
                label: default_fan_label(index),
                mode: suggested_adaptive_mode(fan_info),
            });
        }
    }

    fn normalize(&mut self) -> Result<()> {
        if self.refresh_interval_ms == 0 {
            self.refresh_interval_ms = default_refresh_interval_ms();
        }

        for fan in &mut self.fans {
            if fan.label.trim().is_empty() {
                fan.label = "Fan".to_owned();
            }

            if let FanControlMode::Adaptive {
                max_temp_c,
                hysteresis_c,
                ..
            } = &mut fan.mode
            {
                if *max_temp_c <= 0.0 {
                    *max_temp_c = default_max_temp_c();
                }
                if *hysteresis_c < 0.0 {
                    *hysteresis_c = default_hysteresis_c();
                }
            } else if let FanControlMode::Curve { points, .. } = &mut fan.mode {
                normalize_curve_points(points)?;
            }
        }

        Ok(())
    }
}

impl FanSettings {
    pub fn set_adaptive_default(&mut self, fan_info: &FanInfo) {
        self.mode = suggested_adaptive_mode(fan_info);
    }
}

pub fn suggested_adaptive_mode(_fan_info: &FanInfo) -> FanControlMode {
    FanControlMode::Adaptive {
        target: default_target_selector(),
        max_temp_c: default_max_temp_c(),
        hysteresis_c: default_hysteresis_c(),
    }
}

fn default_fan_label(index: usize) -> String {
    format!("Fan {}", index + 1)
}

const fn default_refresh_interval_ms() -> u64 {
    1_500
}

fn default_target_selector() -> TargetSelector {
    TargetSelector {
        sensor: None,
        group: Some("all_cpu_candidates".to_owned()),
        reduce: crate::config::ReduceOp::Max,
    }
}

const fn default_max_temp_c() -> f64 {
    75.0
}

const fn default_hysteresis_c() -> f64 {
    2.0
}
