use anyhow::{Result, anyhow, bail};
use apple_silicon_fan_control::{
    config::{CurveConfig, HysteresisState},
    control_backend::ControlBackend,
    fan_control::{FanControlAction, FanControlPlan},
    helper_client::HelperClient,
    helper_install::{helper_install_status, install_helper, uninstall_helper},
    platform::detect_system_info,
    runtime::{
        format_snapshots, read_sensor_snapshots, reduce_target_temperature, resolve_target_sensors,
    },
    sensor_profile::profile_for_model,
    smc_controller::{AppleSmc, FanInfo, KeyReading},
};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, thread, time::Duration};

#[derive(Parser, Debug)]
#[command(
    name = "apple-silicon-fan-control-cli",
    version,
    about = "Experimental Apple Silicon fan control CLI for macOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Doctor,
    Probe,
    DumpKeys {
        #[arg(long, default_value = "T")]
        prefix: String,
        #[arg(long, default_value_t = 120)]
        limit: usize,
    },
    Watch {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        once: bool,
    },
    SetRpm {
        #[arg(long)]
        fan: Option<usize>,
        #[arg(long, value_delimiter = ',')]
        fan_indices: Vec<usize>,
        #[arg(long)]
        rpm: u32,
    },
    Auto {
        #[arg(long)]
        fan: Option<usize>,
        #[arg(long, value_delimiter = ',')]
        fan_indices: Vec<usize>,
    },
    Helper {
        #[command(subcommand)]
        command: HelperCommand,
    },
}

#[derive(Subcommand, Debug)]
enum HelperCommand {
    Status,
    Install {
        #[arg(long)]
        helper_binary: Option<PathBuf>,
    },
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Doctor => run_doctor(),
        Command::Probe => run_probe(),
        Command::DumpKeys { prefix, limit } => run_dump_keys(&prefix, limit),
        Command::Watch {
            config,
            apply,
            once,
        } => run_watch(&config, apply, once),
        Command::SetRpm {
            fan,
            fan_indices,
            rpm,
        } => run_set_rpm(fan, &fan_indices, rpm),
        Command::Auto { fan, fan_indices } => run_auto(fan, &fan_indices),
        Command::Helper { command } => run_helper(command),
    }
}

fn run_doctor() -> Result<()> {
    let system = detect_system_info()?;
    let smc = AppleSmc::connect()?;

    println!("System");
    println!("  macOS: {} ({})", system.macos_version, system.macos_build);
    if let Some(model_name) = &system.model_name {
        println!("  model: {} / {}", model_name, system.model_identifier);
    } else {
        println!("  model: {}", system.model_identifier);
    }
    if let Some(chip) = &system.chip {
        println!("  chip: {chip}");
    }
    if let Some(total_cores) = &system.total_cores {
        println!("  cores: {total_cores}");
    }
    let backend = ControlBackend::detect();
    println!("  live_write_backend: {}", backend.label());
    let helper_state = helper_install_status().unwrap_or_else(|err| format!("unknown ({err})"));
    println!("  helper_status: {helper_state}");

    let fan_count = smc.fan_count()?;
    println!("  fan_count: {fan_count}");

    for index in 0..fan_count as usize {
        let fan = smc.read_fan_info(index)?;
        print_fan_info(&fan);
    }

    if fan_count > 0 {
        let ftst = smc.read_key("Ftst").ok();
        let fan_mode = smc.read_key("F0Md").ok();
        let fan_target = smc.read_key("F0Tg").ok();
        println!("SMC control keys");
        if let Some(reading) = ftst {
            print_key_reading("  Ftst", &reading);
        }
        if let Some(reading) = fan_mode {
            print_key_reading("  F0Md", &reading);
        }
        if let Some(reading) = fan_target {
            print_key_reading("  F0Tg", &reading);
        }
    }

    match profile_for_model(&system.model_identifier) {
        Some(profile) => {
            println!("Built-in profile");
            println!("  title: {}", profile.title);
            println!("  sensor_count: {}", profile.sensors.len());
            println!("  groups: {}", profile.supported_groups().join(", "));
            for note in profile.notes {
                println!("  note: {note}");
            }
        }
        None => {
            println!("Built-in profile");
            println!("  none for {}", system.model_identifier);
        }
    }

    Ok(())
}

fn run_probe() -> Result<()> {
    let system = detect_system_info()?;
    let profile = profile_for_model(&system.model_identifier).ok_or_else(|| {
        anyhow!(
            "no built-in sensor profile for `{}`; use `dump-keys --prefix T` and target raw keys in YAML",
            system.model_identifier
        )
    })?;
    let smc = AppleSmc::connect()?;

    println!("Profile: {}", profile.title);
    println!("Model: {}", profile.model_identifier);
    println!();

    let sensors = apple_silicon_fan_control::runtime::resolve_profile_sensors(profile);
    for snapshot in read_sensor_snapshots(&smc, &sensors)? {
        println!(
            "{:<12} {:<4} {:>6.2} C",
            snapshot.label, snapshot.key, snapshot.temp_c
        );
    }

    Ok(())
}

fn run_dump_keys(prefix: &str, limit: usize) -> Result<()> {
    let smc = AppleSmc::connect()?;
    let readings = smc.list_keys_with_prefix(prefix, limit)?;

    for reading in readings {
        print_key_reading("", &reading);
    }

    Ok(())
}

fn run_watch(config_path: &PathBuf, apply: bool, once: bool) -> Result<()> {
    let config = CurveConfig::load(config_path)?;
    let system = detect_system_info()?;
    let profile = profile_for_model(&system.model_identifier);
    let resolved_sensors = resolve_target_sensors(&config.target, profile)?;
    let smc = AppleSmc::connect()?;
    let mut hysteresis = HysteresisState::default();
    let backend = if apply {
        Some(detect_write_backend()?)
    } else {
        None
    };

    println!("Watching {}", config.describe_target());
    println!("Fans: {:?}", config.fan_indices());
    println!("Config: {}", config_path.display());
    println!("Mode: {}", if apply { "apply" } else { "dry-run" });
    println!();

    loop {
        let snapshots = read_sensor_snapshots(&smc, &resolved_sensors)?;
        let target_temp =
            reduce_target_temperature(&config.target, &snapshots).ok_or_else(|| {
                anyhow!(
                    "no sensor values available for {}",
                    config.describe_target()
                )
            })?;
        let curve_rpm = config.interpolate_rpm(target_temp);
        let stable_rpm = hysteresis.apply(target_temp, curve_rpm, config.hysteresis_c);
        let mut actions = Vec::new();
        let mut fan_outputs = Vec::new();

        for fan_index in config.fan_indices() {
            let fan = smc.read_fan_info(*fan_index)?;
            let clamped_rpm = fan.clamp_rpm(stable_rpm);
            fan_outputs.push(format!(
                "fan={} mode={} requested={} clamped={}",
                fan.index,
                fan.mode_label(),
                stable_rpm,
                clamped_rpm
            ));
            actions.push(FanControlAction::SetTargetRpm {
                fan_index: *fan_index,
                rpm: clamped_rpm,
            });
        }

        println!(
            "fans={:?} temp={:.2}C curve_rpm={} outputs=[{}] samples=[{}]",
            config.fan_indices(),
            target_temp,
            stable_rpm,
            fan_outputs.join("; "),
            format_snapshots(&snapshots)
        );

        if let Some(backend) = &backend {
            backend.apply_plan(&smc, &FanControlPlan::new(actions))?;
            println!("applied to fans {:?}", config.fan_indices());
        }

        if once {
            break;
        }

        thread::sleep(Duration::from_millis(config.sample_interval_ms));
    }

    Ok(())
}

fn run_set_rpm(fan: Option<usize>, fan_indices: &[usize], rpm: u32) -> Result<()> {
    let backend = detect_write_backend()?;
    let smc = AppleSmc::connect()?;
    let fan_indices = resolve_command_fan_indices(fan, fan_indices);
    let mut actions = Vec::new();

    for fan_index in &fan_indices {
        let fan = smc.read_fan_info(*fan_index)?;
        let clamped_rpm = fan.clamp_rpm(rpm);
        actions.push(FanControlAction::SetTargetRpm {
            fan_index: *fan_index,
            rpm: clamped_rpm,
        });
        println!(
            "fan={} requested_rpm={} clamped_rpm={} mode_before={}",
            fan_index,
            rpm,
            clamped_rpm,
            fan.mode_label()
        );
    }

    backend.apply_plan(&smc, &FanControlPlan::new(actions))?;
    Ok(())
}

fn run_auto(fan: Option<usize>, fan_indices: &[usize]) -> Result<()> {
    let backend = detect_write_backend()?;
    let smc = AppleSmc::connect()?;
    let fan_indices = resolve_command_fan_indices(fan, fan_indices);
    let actions = fan_indices
        .iter()
        .map(|fan_index| FanControlAction::Auto {
            fan_index: *fan_index,
        })
        .collect();
    backend.apply_plan(&smc, &FanControlPlan::new(actions))?;
    println!("fans {:?} returned to automatic mode", fan_indices);
    Ok(())
}

fn run_helper(command: HelperCommand) -> Result<()> {
    match command {
        HelperCommand::Status => {
            println!("{}", helper_install_status()?);
            let client = HelperClient::system();
            if let Ok(status) = client.read_status() {
                println!("helper_version={}", status.version);
            }
            Ok(())
        }
        HelperCommand::Install { helper_binary } => {
            install_helper(helper_binary.as_deref())?;
            println!("helper installed");
            println!("{}", helper_install_status()?);
            Ok(())
        }
        HelperCommand::Uninstall => {
            uninstall_helper()?;
            println!("helper uninstalled");
            Ok(())
        }
    }
}

fn print_fan_info(fan: &FanInfo) {
    println!(
        "Fan {}: actual={:.0} min={:.0} max={:.0} target={:.0} safe={} mode={} target_type={}",
        fan.index,
        fan.actual_rpm,
        fan.min_rpm,
        fan.max_rpm,
        fan.target_rpm,
        fan.safe_rpm
            .map(|value| format!("{value:.0}"))
            .unwrap_or_else(|| "n/a".to_owned()),
        fan.mode_label(),
        fan.target_data_type
    );
}

fn print_key_reading(prefix: &str, reading: &KeyReading) {
    let bytes = reading
        .bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("");

    if prefix.is_empty() {
        println!(
            "{:<4} type={:<4} size={} numeric={} bytes={}",
            reading.key,
            reading.data_type,
            reading.data_size,
            reading
                .numeric
                .map(|value| format!("{value:.4}"))
                .unwrap_or_else(|| "n/a".to_owned()),
            bytes
        );
    } else {
        println!(
            "{} type={} size={} numeric={} bytes={}",
            prefix,
            reading.data_type,
            reading.data_size,
            reading
                .numeric
                .map(|value| format!("{value:.4}"))
                .unwrap_or_else(|| "n/a".to_owned()),
            bytes
        );
    }
}

fn detect_write_backend() -> Result<ControlBackend> {
    let backend = ControlBackend::detect();
    if backend.can_write() {
        Ok(backend)
    } else {
        bail!(
            "live fan control requires either root privileges or a running privileged helper; install the helper or run with sudo"
        )
    }
}

fn resolve_command_fan_indices(fan: Option<usize>, fan_indices: &[usize]) -> Vec<usize> {
    if !fan_indices.is_empty() {
        return fan_indices.to_vec();
    }

    fan.map(|index| vec![index]).unwrap_or_else(|| vec![0])
}
