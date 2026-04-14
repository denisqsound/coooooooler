use anyhow::{Context, Result, anyhow};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub model_name: Option<String>,
    pub model_identifier: String,
    pub chip: Option<String>,
    pub total_cores: Option<String>,
    pub macos_version: String,
    pub macos_build: String,
}

pub fn detect_system_info() -> Result<SystemInfo> {
    let macos_version = run_command("sw_vers", &["-productVersion"])?;
    let macos_build = run_command("sw_vers", &["-buildVersion"])?;
    let hardware = run_command("system_profiler", &["SPHardwareDataType"])?;

    let mut model_name = None;
    let mut model_identifier = None;
    let mut chip = None;
    let mut total_cores = None;

    for line in hardware.lines() {
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };

        let value = value.trim().to_owned();
        match key.trim() {
            "Model Name" => model_name = Some(value),
            "Model Identifier" => model_identifier = Some(value),
            "Chip" => chip = Some(value),
            "Total Number of Cores" => total_cores = Some(value),
            _ => {}
        }
    }

    Ok(SystemInfo {
        model_name,
        model_identifier: model_identifier.ok_or_else(|| {
            anyhow!("failed to parse Model Identifier from system_profiler output")
        })?,
        chip,
        total_cores,
        macos_version,
        macos_build,
    })
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn run_command(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `{program}`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`{program}` exited with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
