use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, fs, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CurveConfig {
    #[serde(default)]
    pub fan_index: Option<usize>,
    #[serde(default)]
    pub fan_indices: Vec<usize>,
    #[serde(default = "default_sample_interval_ms")]
    pub sample_interval_ms: u64,
    #[serde(default = "default_hysteresis_c")]
    pub hysteresis_c: f64,
    pub target: TargetSelector,
    pub points: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TargetSelector {
    pub sensor: Option<String>,
    pub group: Option<String>,
    #[serde(default)]
    pub reduce: ReduceOp,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReduceOp {
    Min,
    #[default]
    Max,
    Average,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CurvePoint {
    pub temp_c: f64,
    pub rpm: u32,
}

#[derive(Debug, Clone, Default)]
pub struct HysteresisState {
    last_temp_c: Option<f64>,
    last_rpm: Option<u32>,
}

impl CurveConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file `{}`", path.display()))?;
        let mut config: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse config file `{}`", path.display()))?;
        config.normalize()?;
        Ok(config)
    }

    fn normalize(&mut self) -> Result<()> {
        if self.fan_indices.is_empty() {
            self.fan_indices
                .push(self.fan_index.unwrap_or_else(default_fan_index));
        }
        self.fan_indices.sort_unstable();
        self.fan_indices.dedup();

        validate_target(&self.target)?;
        ensure!(
            !self.points.is_empty(),
            "config must contain at least one curve point"
        );
        ensure!(
            self.sample_interval_ms > 0,
            "sample_interval_ms must be greater than zero",
        );

        normalize_curve_points(&mut self.points)?;

        Ok(())
    }

    pub fn fan_indices(&self) -> &[usize] {
        &self.fan_indices
    }

    pub fn describe_target(&self) -> String {
        if let Some(sensor) = &self.target.sensor {
            format!("sensor `{sensor}`")
        } else if let Some(group) = &self.target.group {
            format!("group `{group}` ({})", self.target.reduce.label())
        } else {
            "unknown target".to_owned()
        }
    }

    pub fn interpolate_rpm(&self, temp_c: f64) -> u32 {
        interpolate_curve_points(&self.points, temp_c).unwrap_or_default()
    }
}

impl TargetSelector {
    pub fn reduce(&self, values: &[f64]) -> Option<f64> {
        self.reduce.apply(values)
    }
}

impl ReduceOp {
    pub fn apply(self, values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            return None;
        }

        match self {
            Self::Min => values.iter().copied().reduce(f64::min),
            Self::Max => values.iter().copied().reduce(f64::max),
            Self::Average => Some(values.iter().sum::<f64>() / values.len() as f64),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
            Self::Average => "average",
        }
    }
}

impl TargetSelector {
    pub fn describe(&self) -> String {
        if let Some(sensor) = &self.sensor {
            format!("sensor `{sensor}`")
        } else if let Some(group) = &self.group {
            format!("group `{group}` ({})", self.reduce.label())
        } else {
            "unknown target".to_owned()
        }
    }
}

impl HysteresisState {
    pub fn apply(&mut self, temp_c: f64, candidate_rpm: u32, hysteresis_c: f64) -> u32 {
        let final_rpm = match (self.last_temp_c, self.last_rpm) {
            (Some(last_temp), Some(last_rpm))
                if candidate_rpm < last_rpm && temp_c + hysteresis_c >= last_temp =>
            {
                last_rpm
            }
            _ => candidate_rpm,
        };

        self.last_temp_c = Some(temp_c);
        self.last_rpm = Some(final_rpm);
        final_rpm
    }
}

pub fn validate_target(target: &TargetSelector) -> Result<()> {
    ensure!(
        target.sensor.is_some() ^ target.group.is_some(),
        "config target must specify exactly one of `sensor` or `group`",
    );
    Ok(())
}

pub fn normalize_curve_points(points: &mut Vec<CurvePoint>) -> Result<()> {
    ensure!(!points.is_empty(), "curve must contain at least one point");

    points.sort_by(|left, right| {
        left.temp_c
            .partial_cmp(&right.temp_c)
            .unwrap_or(Ordering::Equal)
    });

    let mut last_temp = None;
    for point in points.iter() {
        if let Some(previous) = last_temp {
            ensure!(
                point.temp_c >= previous,
                "curve points must be ordered by non-decreasing temp_c",
            );
        }
        last_temp = Some(point.temp_c);
    }

    Ok(())
}

pub fn interpolate_curve_points(points: &[CurvePoint], temp_c: f64) -> Option<u32> {
    if points.is_empty() {
        return None;
    }

    if points.len() == 1 {
        return Some(points[0].rpm);
    }

    if temp_c <= points[0].temp_c {
        return Some(points[0].rpm);
    }

    for window in points.windows(2) {
        let left = &window[0];
        let right = &window[1];
        if temp_c <= right.temp_c {
            if (right.temp_c - left.temp_c).abs() < f64::EPSILON {
                return Some(right.rpm);
            }

            let position = (temp_c - left.temp_c) / (right.temp_c - left.temp_c);
            let rpm =
                f64::from(left.rpm) + ((f64::from(right.rpm) - f64::from(left.rpm)) * position);
            return Some(rpm.round() as u32);
        }
    }

    points.last().map(|point| point.rpm)
}

const fn default_fan_index() -> usize {
    0
}

const fn default_sample_interval_ms() -> u64 {
    1_500
}

const fn default_hysteresis_c() -> f64 {
    2.0
}

#[cfg(test)]
mod tests {
    use super::{CurveConfig, CurvePoint, HysteresisState, ReduceOp, TargetSelector};

    fn config() -> CurveConfig {
        CurveConfig {
            fan_index: Some(0),
            fan_indices: Vec::new(),
            sample_interval_ms: 1000,
            hysteresis_c: 2.0,
            target: TargetSelector {
                sensor: Some("Tf04".to_owned()),
                group: None,
                reduce: ReduceOp::Max,
            },
            points: vec![
                CurvePoint {
                    temp_c: 40.0,
                    rpm: 1300,
                },
                CurvePoint {
                    temp_c: 60.0,
                    rpm: 2600,
                },
                CurvePoint {
                    temp_c: 80.0,
                    rpm: 5200,
                },
            ],
        }
    }

    #[test]
    fn interpolates_between_points() {
        let config = config();
        assert_eq!(config.interpolate_rpm(40.0), 1300);
        assert_eq!(config.interpolate_rpm(50.0), 1950);
        assert_eq!(config.interpolate_rpm(70.0), 3900);
        assert_eq!(config.interpolate_rpm(90.0), 5200);
    }

    #[test]
    fn hysteresis_holds_lower_rpm_briefly() {
        let mut state = HysteresisState::default();
        assert_eq!(state.apply(70.0, 3200, 2.0), 3200);
        assert_eq!(state.apply(69.0, 3000, 2.0), 3200);
        assert_eq!(state.apply(66.5, 2800, 2.0), 2800);
    }
}
