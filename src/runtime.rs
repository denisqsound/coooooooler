use crate::{config::TargetSelector, sensor_profile::SensorProfile, smc_controller::AppleSmc};
use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone)]
pub struct ResolvedSensor {
    pub label: String,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct SensorSnapshot {
    pub label: String,
    pub key: String,
    pub temp_c: f64,
}

pub fn resolve_target_sensors(
    target: &TargetSelector,
    profile: Option<&'static SensorProfile>,
) -> Result<Vec<ResolvedSensor>> {
    if let Some(sensor) = &target.sensor {
        return Ok(vec![resolve_sensor(sensor, profile)?]);
    }

    let group = target
        .group
        .as_deref()
        .ok_or_else(|| anyhow!("config target group is missing"))?;
    let profile = profile.ok_or_else(|| {
        anyhow!("no built-in profile available for group `{group}`; use a raw 4-character sensor key instead")
    })?;
    let sensors = profile
        .sensors_for_group(group)
        .ok_or_else(|| anyhow!("unknown profile group `{group}`"))?;

    Ok(sensors
        .into_iter()
        .map(|sensor| ResolvedSensor {
            label: sensor.label.to_owned(),
            key: sensor.key.to_owned(),
        })
        .collect())
}

pub fn resolve_profile_sensors(profile: &'static SensorProfile) -> Vec<ResolvedSensor> {
    profile
        .sensors
        .iter()
        .map(|sensor| ResolvedSensor {
            label: sensor.label.to_owned(),
            key: sensor.key.to_owned(),
        })
        .collect()
}

pub fn read_sensor_snapshots(
    smc: &AppleSmc,
    sensors: &[ResolvedSensor],
) -> Result<Vec<SensorSnapshot>> {
    let mut snapshots = Vec::with_capacity(sensors.len());

    for sensor in sensors {
        let temp_c = smc.read_temperature_c(&sensor.key).with_context(|| {
            format!("failed to read sensor `{}` ({})", sensor.label, sensor.key)
        })?;
        snapshots.push(SensorSnapshot {
            label: sensor.label.clone(),
            key: sensor.key.clone(),
            temp_c,
        });
    }

    Ok(snapshots)
}

pub fn read_sensor_snapshots_best_effort(
    smc: &AppleSmc,
    sensors: &[ResolvedSensor],
) -> Vec<SensorSnapshot> {
    sensors
        .iter()
        .filter_map(|sensor| {
            smc.read_temperature_c(&sensor.key)
                .ok()
                .map(|temp_c| SensorSnapshot {
                    label: sensor.label.clone(),
                    key: sensor.key.clone(),
                    temp_c,
                })
        })
        .collect()
}

pub fn reduce_target_temperature(
    target: &TargetSelector,
    snapshots: &[SensorSnapshot],
) -> Option<f64> {
    let values: Vec<f64> = snapshots.iter().map(|snapshot| snapshot.temp_c).collect();
    target.reduce(&values)
}

pub fn format_snapshots(snapshots: &[SensorSnapshot]) -> String {
    snapshots
        .iter()
        .map(|snapshot| {
            format!(
                "{} / {} = {:.2}C",
                snapshot.label, snapshot.key, snapshot.temp_c
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn resolve_sensor(
    label_or_key: &str,
    profile: Option<&'static SensorProfile>,
) -> Result<ResolvedSensor> {
    if let Some(profile) = profile {
        if let Some(sensor) = profile.find_sensor(label_or_key) {
            return Ok(ResolvedSensor {
                label: sensor.label.to_owned(),
                key: sensor.key.to_owned(),
            });
        }
    }

    if label_or_key.len() == 4 {
        return Ok(ResolvedSensor {
            label: label_or_key.to_owned(),
            key: label_or_key.to_owned(),
        });
    }

    bail!(
        "sensor `{label_or_key}` was not found in the built-in profile and is not a raw 4-character SMC key"
    )
}
