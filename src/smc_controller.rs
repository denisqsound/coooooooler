use anyhow::{Result, anyhow, bail};
use smc_lib::{
    io::{IOService, err_str},
    structs::SMCVal,
    value::SmcValue,
};
use std::{thread, time::Duration, time::Instant};

pub struct AppleSmc {
    service: IOService,
}

#[derive(Debug, Clone)]
pub struct KeyReading {
    pub key: String,
    pub data_type: String,
    pub data_size: u32,
    pub bytes: Vec<u8>,
    pub numeric: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FanInfo {
    pub index: usize,
    pub actual_rpm: f64,
    pub min_rpm: f64,
    pub max_rpm: f64,
    pub safe_rpm: Option<f64>,
    pub target_rpm: f64,
    pub mode: u8,
    pub target_data_type: String,
}

impl AppleSmc {
    pub fn connect() -> Result<Self> {
        let service = IOService::init().map_err(|err| anyhow!(err.to_string()))?;
        Ok(Self { service })
    }

    pub fn read_key(&self, key: &str) -> Result<KeyReading> {
        let raw = self.read_raw(key)?;
        Ok(self.decode_reading(raw))
    }

    pub fn read_temperature_c(&self, key: &str) -> Result<f64> {
        let raw = self.read_raw(key)?;
        decode_temperature(raw).ok_or_else(|| anyhow!("failed to decode temperature key `{key}`"))
    }

    pub fn fan_count(&self) -> Result<u8> {
        self.read_u8_like("FNum")
    }

    pub fn read_all_fans(&self) -> Result<Vec<FanInfo>> {
        let fan_count = self.fan_count()? as usize;
        let mut fans = Vec::with_capacity(fan_count);
        for index in 0..fan_count {
            fans.push(self.read_fan_info(index)?);
        }
        Ok(fans)
    }

    pub fn read_fan_info(&self, index: usize) -> Result<FanInfo> {
        let actual_key = fan_key(index, "Ac");
        let min_key = fan_key(index, "Mn");
        let max_key = fan_key(index, "Mx");
        let safe_key = fan_key(index, "Sf");
        let target_key = fan_key(index, "Tg");
        let mode_key = fan_key(index, "Md");

        let actual_rpm = self.read_rpm(&actual_key)?;
        let min_rpm = self.read_rpm(&min_key)?;
        let max_rpm = self.read_rpm(&max_key)?;
        let safe_rpm = self.read_rpm(&safe_key).ok();
        let target_raw = self.read_raw(&target_key)?;
        let target_rpm = decode_rpm(target_raw)
            .ok_or_else(|| anyhow!("failed to decode fan target key `{target_key}`"))?;
        let mode = self.read_u8_like(&mode_key)?;

        Ok(FanInfo {
            index,
            actual_rpm,
            min_rpm,
            max_rpm,
            safe_rpm,
            target_rpm,
            mode,
            target_data_type: data_type_name(&target_raw),
        })
    }

    pub fn list_keys_with_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<KeyReading>> {
        let mut readings = Vec::new();

        for value in self
            .service
            .values_iter()
            .map_err(|err| anyhow!("failed to iterate SMC keys: {}", err_str(err)))?
        {
            let Ok(raw) = value else {
                continue;
            };

            let key = raw.key_str().to_string();
            if !key.starts_with(prefix) {
                continue;
            }

            readings.push(self.decode_reading(raw));
            if readings.len() >= limit {
                break;
            }
        }

        Ok(readings)
    }

    pub fn ensure_manual_control(&self, fan_index: usize) -> Result<()> {
        self.ensure_manual_control_for_fans(&[fan_index])
    }

    pub fn ensure_manual_control_for_fans(&self, fan_indices: &[usize]) -> Result<()> {
        if fan_indices.is_empty() {
            return Ok(());
        }

        self.write_u8("Ftst", 1)?;
        let deadline = Instant::now() + Duration::from_secs(10);

        while Instant::now() < deadline {
            let all_ready = fan_indices.iter().all(|fan_index| {
                self.read_u8_like(&fan_key(*fan_index, "Md"))
                    .map(|mode| mode == 0 || mode == 1)
                    .unwrap_or(false)
            });

            if all_ready {
                for fan_index in fan_indices {
                    self.write_u8(&fan_key(*fan_index, "Md"), 1)?;
                }
                return Ok(());
            }

            thread::sleep(Duration::from_millis(100));
        }

        let failed = fan_indices
            .iter()
            .map(|fan_index| {
                let mode = self.read_u8_like(&fan_key(*fan_index, "Md")).unwrap_or(255);
                format!("{fan_index}:{mode}")
            })
            .collect::<Vec<_>>()
            .join(", ");

        bail!("manual fan control unlock timed out; fan modes remained [{failed}]")
    }

    pub fn set_target_rpm(&self, fan_index: usize, rpm: u32) -> Result<()> {
        let target_key = fan_key(fan_index, "Tg");
        let raw = self.read_raw(&target_key)?;
        let bytes = encode_rpm(raw, rpm as f64)?;
        let key = key_bytes(&target_key)?;
        self.service
            .write_key(&key, &bytes)
            .map_err(|err| anyhow!("failed to write `{target_key}`: {}", err_str(err)))?;
        Ok(())
    }

    pub fn set_auto_mode(&self, fan_index: usize) -> Result<()> {
        self.set_auto_mode_without_releasing_test_mode(fan_index)?;
        self.release_test_mode()?;
        Ok(())
    }

    pub fn set_auto_mode_without_releasing_test_mode(&self, fan_index: usize) -> Result<()> {
        self.write_u8(&fan_key(fan_index, "Md"), 0)
    }

    pub fn release_test_mode(&self) -> Result<()> {
        self.write_u8("Ftst", 0)
    }

    fn read_raw(&self, key: &str) -> Result<SMCVal> {
        let key_bytes = key_bytes(key)?;
        self.service
            .read_key(&key_bytes)
            .map_err(|err| anyhow!("failed to read `{key}`: {}", err_str(err)))
    }

    fn read_u8_like(&self, key: &str) -> Result<u8> {
        let raw = self.read_raw(key)?;
        match raw.data_value() {
            Some(SmcValue::U8(value)) => Ok(value),
            Some(SmcValue::I8(value)) if value >= 0 => Ok(value as u8),
            Some(SmcValue::Bool(value)) => Ok(u8::from(value)),
            Some(SmcValue::U16(value)) if u8::try_from(value).is_ok() => Ok(value as u8),
            Some(SmcValue::I16(value)) if (0..=u8::MAX as i16).contains(&value) => Ok(value as u8),
            Some(other) => bail!("key `{key}` is not an 8-bit value: {other}"),
            None => bail!(
                "key `{key}` has an unsupported type `{}`",
                data_type_name(&raw)
            ),
        }
    }

    fn read_rpm(&self, key: &str) -> Result<f64> {
        let raw = self.read_raw(key)?;
        decode_rpm(raw).ok_or_else(|| anyhow!("failed to decode RPM key `{key}`"))
    }

    fn write_u8(&self, key: &str, value: u8) -> Result<()> {
        let key_bytes = key_bytes(key)?;
        self.service
            .write_key(&key_bytes, &[value])
            .map_err(|err| anyhow!("failed to write `{key}`: {}", err_str(err)))
    }

    fn decode_reading(&self, raw: SMCVal) -> KeyReading {
        KeyReading {
            key: raw.key_str().to_string(),
            data_type: data_type_name(&raw),
            data_size: raw.data_size,
            bytes: raw.valid_bytes().to_vec(),
            numeric: decode_numeric(&raw),
        }
    }
}

impl FanInfo {
    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            0 => "auto",
            1 => "manual",
            3 => "system",
            _ => "unknown",
        }
    }

    pub fn clamp_rpm(&self, rpm: u32) -> u32 {
        let min_rpm = self.min_rpm.max(1.0).round() as u32;
        let max_rpm = self.max_rpm.round() as u32;
        rpm.clamp(min_rpm, max_rpm.max(min_rpm))
    }
}

fn key_bytes(key: &str) -> Result<[u8; 4]> {
    let bytes = key.as_bytes();
    if bytes.len() != 4 {
        bail!("SMC key `{key}` must be exactly 4 ASCII characters");
    }

    let mut key_array = [0_u8; 4];
    key_array.copy_from_slice(bytes);
    Ok(key_array)
}

fn fan_key(index: usize, suffix: &str) -> String {
    format!("F{index}{suffix}")
}

fn data_type_name(raw: &SMCVal) -> String {
    raw.data_type_str().trim().to_owned()
}

fn decode_temperature(raw: SMCVal) -> Option<f64> {
    match raw.data_value()? {
        SmcValue::F32 { le, be } => pick_float(le, be, 0.0, 150.0),
        SmcValue::U8(value) => Some(value as f64),
        SmcValue::I8(value) => Some(value as f64),
        SmcValue::U16(value) => Some(value as f64),
        SmcValue::I16(value) => Some(value as f64),
        SmcValue::U32(value) => Some(value as f64),
        SmcValue::I32(value) => Some(value as f64),
        SmcValue::Ioft48_16(raw) => Some(((raw >> 16) as f64) + ((raw & 0xFFFF) as f64 / 65536.0)),
        _ => None,
    }
}

fn decode_rpm(raw: SMCVal) -> Option<f64> {
    match raw.data_value()? {
        SmcValue::F32 { le, be } => pick_float(le, be, 0.0, 20_000.0),
        SmcValue::U16(value) => Some(value as f64),
        SmcValue::I16(value) if value >= 0 => Some(value as f64),
        SmcValue::U32(value) => Some(value as f64),
        SmcValue::I32(value) if value >= 0 => Some(value as f64),
        SmcValue::U8(value) => Some(value as f64),
        _ => None,
    }
}

fn decode_numeric(raw: &SMCVal) -> Option<f64> {
    match raw.data_value()? {
        SmcValue::F32 { le, be } => pick_float(le, be, -1_000_000.0, 1_000_000.0),
        SmcValue::U8(value) => Some(value as f64),
        SmcValue::I8(value) => Some(value as f64),
        SmcValue::U16(value) => Some(value as f64),
        SmcValue::I16(value) => Some(value as f64),
        SmcValue::U32(value) => Some(value as f64),
        SmcValue::I32(value) => Some(value as f64),
        SmcValue::U64(value) => Some(value as f64),
        SmcValue::I64(value) => Some(value as f64),
        SmcValue::Bool(value) => Some(u8::from(value) as f64),
        SmcValue::Ioft48_16(raw) => Some(((raw >> 16) as f64) + ((raw & 0xFFFF) as f64 / 65536.0)),
        SmcValue::Chars(_) => None,
    }
}

fn encode_rpm(raw: SMCVal, rpm: f64) -> Result<Vec<u8>> {
    let Some(value) = raw.data_value() else {
        bail!(
            "fan target key `{}` uses unsupported data type `{}`",
            raw.key_str(),
            data_type_name(&raw)
        );
    };

    let rounded = rpm.round();
    if !(0.0..=20_000.0).contains(&rounded) {
        bail!("RPM {rounded} is outside the supported range");
    }

    match value {
        SmcValue::F32 { le, be } => {
            let use_le = matches!(pick_float(le, be, 0.0, 20_000.0), Some(selected) if (selected - f64::from(le)).abs() < 0.0001);
            let bytes = if use_le {
                (rounded as f32).to_le_bytes().to_vec()
            } else {
                (rounded as f32).to_be_bytes().to_vec()
            };
            Ok(bytes)
        }
        SmcValue::U16(_) => Ok((rounded as u16).to_le_bytes().to_vec()),
        SmcValue::I16(_) => Ok((rounded as i16).to_le_bytes().to_vec()),
        SmcValue::U32(_) => Ok((rounded as u32).to_le_bytes().to_vec()),
        SmcValue::I32(_) => Ok((rounded as i32).to_le_bytes().to_vec()),
        other => bail!(
            "fan target key `{}` uses unsupported numeric type `{other}`",
            raw.key_str()
        ),
    }
}

fn pick_float(le: f32, be: f32, min: f64, max: f64) -> Option<f64> {
    let le = f64::from(le);
    let be = f64::from(be);
    let plausible = |value: f64| value.is_finite() && (min..=max).contains(&value);

    match (plausible(le), plausible(be)) {
        (true, false) => Some(le),
        (false, true) => Some(be),
        (true, true) => {
            let le_tiny = le.abs() < 0.5;
            let be_tiny = be.abs() < 0.5;

            match (le_tiny, be_tiny) {
                (true, false) => Some(be),
                (false, true) => Some(le),
                _ => Some(if le.abs() >= be.abs() { le } else { be }),
            }
        }
        (false, false) => None,
    }
}
