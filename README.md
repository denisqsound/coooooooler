# Apple Silicon Fan Control

A native macOS application for controlling fans on Apple Silicon Macs via SMC. Includes both a GUI (eframe) and a CLI.

## Features

- Read temperatures from SMC sensors (CPU, GPU, etc.)
- Set fan speed manually or use automatic curve-based control
- YAML-based fan curve configuration with hysteresis
- Privileged helper for fan writes without running as root
- Single-instance guard — only one GUI copy runs at a time

## Building

```bash
cargo build --release
```

## Usage

### GUI

```bash
cargo run
```

Or with helper management:

```bash
./apple-silicon-fan-control --install-helper
./apple-silicon-fan-control --uninstall-helper
./apple-silicon-fan-control --helper-status
```

### CLI

```bash
# System and fan diagnostics
cargo run --bin apple-silicon-fan-control-cli -- doctor

# Read sensor temperatures
cargo run --bin apple-silicon-fan-control-cli -- probe

# Dump raw SMC keys
cargo run --bin apple-silicon-fan-control-cli -- dump-keys --prefix T

# Watch temperatures and fan curve (dry-run)
cargo run --bin apple-silicon-fan-control-cli -- watch --config config/mac15_10-m3-max.yaml

# Apply fan curve
cargo run --bin apple-silicon-fan-control-cli -- watch --config config/mac15_10-m3-max.yaml --apply

# Set fan RPM directly
cargo run --bin apple-silicon-fan-control-cli -- set-rpm --fan 0 --rpm 3000

# Return fans to automatic mode
cargo run --bin apple-silicon-fan-control-cli -- auto
```

## Fan Curve Config

Example (`config/mac15_10-m3-max.yaml`):

```yaml
fan_indices: [0, 1]
sample_interval_ms: 1500
hysteresis_c: 2.0

target:
  group: all_cpu_candidates
  reduce: max

points:
  - temp_c: 45.0
    rpm: 1300
  - temp_c: 60.0
    rpm: 2200
  - temp_c: 70.0
    rpm: 3200
  - temp_c: 80.0
    rpm: 4300
  - temp_c: 90.0
    rpm: 5600
```

## Requirements

- macOS on Apple Silicon
- Rust 2024 edition
- Root privileges or installed privileged helper for fan control
