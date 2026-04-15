# Apple Silicon Fan Control

Experimental macOS fan control app for Apple Silicon Macs, written in Rust.

This project focuses on a simple workflow for M-series MacBooks:

- choose the component temperature to watch
- choose the temperature where fan speed should reach maximum
- let the app ramp fan speed smoothly from quiet to max
- switch back to system control at any time
- force maximum cooling when needed

The app currently targets Apple Silicon only. Intel Macs are out of scope.

## Status

This is an experimental utility that talks to Apple SMC and uses a privileged helper for real fan writes.

Use it carefully:

- this is not an official Apple utility
- this is not affiliated with or derived from the commercial Macs Fan Control app
- hardware behavior and sensor mappings may differ between models and macOS versions
- incorrect fan control can lead to noise, battery drain, or thermal behavior you do not want

## Current UI Model

Each fan has three modes:

- `Adaptive`: watch a selected component and smoothly ramp the fan to maximum at a chosen temperature
- `System`: return control to the default macOS thermal policy
- `Max`: force the fan to maximum allowed RPM

The intended main mode is `Adaptive`.

## Supported Platform

- macOS on Apple Silicon
- tested against an M3 Max profile in this repository
- Rust toolchain required for building from source

## Project Layout

- `apple-silicon-fan-control/`: Rust application, CLI, helper, and GUI
- `Taskfile.yml`: convenient tasks for development, install, and autostart
- `apple-silicon-fan-control/config/`: example config files and model-specific data

## Build

From the repository root:

```bash
task dev
```

Or directly:

```bash
cd apple-silicon-fan-control
cargo build --bins
cargo run --bin apple-silicon-fan-control
```

## Installation

From the repository root:

```bash
task install
```

This will:

- build release binaries
- install the GUI app binary to `~/Applications/AppleSiliconFanControl`
- install the helper binary next to it

## Enable Real Fan Control

The GUI can monitor temperatures without privileges, but changing fan RPM requires the privileged helper.

After launching the app:

1. Click `Enable Control`
2. Approve the macOS administrator prompt
3. The helper will be installed and used for live fan writes

You can inspect helper state with:

```bash
task helper:status
```

## Autostart

Enable launch at login:

```bash
task autostart
```

Disable it:

```bash
task autostart:off
```

Remove local installation:

```bash
task uninstall
```

## Other Useful Tasks

```bash
task test
task doctor
```

- `task test`: run Rust tests
- `task doctor`: print model, helper, SMC, and fan diagnostics

## How Adaptive Mode Works

`Adaptive` mode does not require you to hand-edit a full fan curve.

Instead you choose:

- the component or sensor group to track
- the temperature where cooling should reach maximum

The app then builds a smooth curve automatically:

- lower temperatures stay near the fan's quiet minimum
- fan speed ramps up as temperature rises
- maximum RPM is reached at your chosen threshold

## Known Limitations

- sensor mappings are model-specific and may need adjustment for some Macs
- helper installation is currently triggered from the app and depends on macOS administrator approval
- this project is currently source-first and developer-oriented, not yet a polished packaged `.app`
- the repository currently does not include signing, notarization, or a packaged installer

## Publishing Notes

Before making the repository public, review local build artifacts and local machine files.

At minimum, do not publish:

- `apple-silicon-fan-control/target/`
- local terminal logs
- local app install directories under `~/Applications`
- any future private certificates, signing identities, or secret config files

## License

Add the license you want to publish this under before making the repository public.

## Disclaimer

This software is provided as-is. You are responsible for testing it on your own hardware and deciding whether you trust its fan behavior on your machine.
