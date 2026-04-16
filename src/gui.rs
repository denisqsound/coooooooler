use crate::{
    app_settings::{AppSettings, FanControlMode},
    config::{CurvePoint, HysteresisState, ReduceOp, TargetSelector, interpolate_curve_points},
    control_backend::ControlBackend,
    fan_control::{FanControlAction, FanControlPlan},
    helper_client::HelperClient,
    helper_install::{helper_install_status, install_helper, uninstall_helper},
    helper_paths::{helper_binary_path, helper_launch_daemon_plist_path},
    platform::{SystemInfo, detect_system_info, is_root},
    runtime::{
        SensorSnapshot, format_snapshots, read_sensor_snapshots_best_effort,
        reduce_target_temperature, resolve_profile_sensors, resolve_target_sensors,
    },
    sensor_profile::{SensorProfile, profile_for_model},
    single_instance::SingleInstanceGuard,
    smc_controller::{AppleSmc, FanInfo},
};
use anyhow::Context;
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, FontId, Frame, RichText, Sense, Stroke,
    StrokeKind,
};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

const APP_DISPLAY_NAME: &str = "coooooooler";

pub fn run(instance_guard: SingleInstanceGuard) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_DISPLAY_NAME)
            .with_inner_size([1240.0, 880.0])
            .with_min_inner_size([980.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_DISPLAY_NAME,
        options,
        Box::new(|cc| {
            configure_style(&cc.egui_ctx);
            Ok(Box::new(FanControlApp::new(instance_guard)))
        }),
    )
}

struct FanControlApp {
    instance_guard: SingleInstanceGuard,
    smc: Option<AppleSmc>,
    system: Option<SystemInfo>,
    profile: Option<&'static SensorProfile>,
    settings: AppSettings,
    settings_path: Option<PathBuf>,
    settings_loaded: bool,
    settings_dirty: bool,
    runtime: RuntimeState,
    fan_previews: Vec<FanPreview>,
    hysteresis: Vec<HysteresisState>,
    show_sensor_panel: bool,
    sensor_filter: String,
    last_status: Option<String>,
    last_error: Option<String>,
    pending_immediate_refresh: bool,
}

#[derive(Default)]
struct RuntimeState {
    fans: Vec<FanInfo>,
    sensors: Vec<SensorSnapshot>,
    live_write_available: bool,
    control_backend_label: String,
    helper_status: HelperUiStatus,
    last_refresh: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
struct HelperUiStatus {
    installed_binary: bool,
    launch_daemon_installed: bool,
    socket_present: bool,
    reachable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanUiMode {
    Adaptive,
    System,
    MaxAlways,
}

impl HelperUiStatus {
    fn detect() -> Self {
        let client = HelperClient::system();
        Self {
            installed_binary: helper_binary_path().exists(),
            launch_daemon_installed: helper_launch_daemon_plist_path().exists(),
            socket_present: client.socket_path().exists(),
            reachable: client.ping().is_ok(),
        }
    }

    fn is_installed(&self) -> bool {
        self.installed_binary || self.launch_daemon_installed || self.socket_present
    }

    fn summary(&self) -> &'static str {
        if self.reachable {
            "Helper is running"
        } else if self.is_installed() {
            "Helper is installed but not responding"
        } else {
            "Helper is not installed"
        }
    }
}

#[derive(Debug, Clone, Default)]
struct FanPreview {
    target_temp_c: Option<f64>,
    requested_rpm: Option<u32>,
    sample_details: String,
    error: Option<String>,
}

impl FanControlApp {
    fn new(instance_guard: SingleInstanceGuard) -> Self {
        let mut app = Self {
            instance_guard,
            smc: None,
            system: None,
            profile: None,
            settings: AppSettings::default(),
            settings_path: None,
            settings_loaded: false,
            settings_dirty: false,
            runtime: RuntimeState::default(),
            fan_previews: Vec::new(),
            hysteresis: Vec::new(),
            show_sensor_panel: false,
            sensor_filter: String::new(),
            last_status: None,
            last_error: None,
            pending_immediate_refresh: false,
        };
        app.bootstrap();
        app
    }

    fn activate_existing_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
        ctx.request_repaint();
        self.last_status = Some("Activated existing window".to_owned());
    }

    fn bootstrap(&mut self) {
        if self.system.is_none() {
            match detect_system_info() {
                Ok(system) => {
                    self.profile = profile_for_model(&system.model_identifier);
                    self.system = Some(system);
                }
                Err(err) => {
                    self.last_error = Some(format!("Failed to detect hardware: {err}"));
                }
            }
        }

        if self.smc.is_none() {
            match AppleSmc::connect() {
                Ok(smc) => self.smc = Some(smc),
                Err(err) => {
                    self.last_error = Some(format!("Failed to connect to AppleSMC: {err}"));
                    return;
                }
            }
        }

        self.refresh_runtime();
    }

    fn refresh_runtime(&mut self) {
        let backend = ControlBackend::detect();
        self.runtime.live_write_available = backend.can_write();
        self.runtime.control_backend_label = backend.label();
        self.runtime.helper_status = HelperUiStatus::detect();
        if self.smc.is_none() {
            self.bootstrap();
            return;
        }

        let fans = match self.smc.as_ref().and_then(|smc| smc.read_all_fans().ok()) {
            Some(fans) => fans,
            None => {
                self.last_error = Some("Failed to read fan state".to_owned());
                self.smc = None;
                return;
            }
        };

        if !self.settings_loaded {
            self.load_settings(&fans);
        } else {
            self.settings.sync_with_fans(&fans);
        }

        self.runtime.fans = fans.clone();
        self.ensure_runtime_len();

        let sensors = match (self.profile, self.smc.as_ref()) {
            (Some(profile), Some(smc)) => {
                let resolved = resolve_profile_sensors(profile);
                read_sensor_snapshots_best_effort(smc, &resolved)
            }
            _ => Vec::new(),
        };

        let mut previews = Vec::with_capacity(fans.len());
        let mut actions = Vec::new();
        for (index, fan) in fans.iter().enumerate() {
            let preview = match self.smc.as_ref() {
                Some(smc) => {
                    let (preview, action) = evaluate_fan_mode(
                        smc,
                        index,
                        fan,
                        self.settings.fans.get(index),
                        self.profile,
                        self.hysteresis.get_mut(index),
                    );
                    if let Some(action) = action {
                        actions.push(action);
                    }
                    preview
                }
                None => FanPreview {
                    error: Some("AppleSMC connection unavailable".to_owned()),
                    ..Default::default()
                },
            };
            previews.push(preview);
        }

        if backend.can_write() && !actions.is_empty() {
            if let Some(smc) = self.smc.as_ref() {
                let plan = FanControlPlan::new(actions);
                if let Err(err) = backend.apply_plan(smc, &plan) {
                    self.last_error = Some(format!("Failed to apply fan plan: {err}"));
                }
            }
        }

        self.runtime.sensors = sensors;
        self.runtime.last_refresh = Some(Instant::now());
        self.fan_previews = previews;
    }

    fn load_settings(&mut self, fans: &[FanInfo]) {
        match AppSettings::settings_path() {
            Ok(path) => {
                self.settings_path = Some(path.clone());
                match AppSettings::load_or_default(&path, fans) {
                    Ok(settings) => {
                        self.settings = settings;
                        self.settings_loaded = true;
                    }
                    Err(err) => {
                        self.settings = AppSettings::default();
                        self.settings.sync_with_fans(fans);
                        self.settings_loaded = true;
                        self.last_error = Some(format!(
                            "Failed to load saved settings, using defaults instead: {err}"
                        ));
                    }
                }
            }
            Err(err) => {
                self.settings = AppSettings::default();
                self.settings.sync_with_fans(fans);
                self.settings_loaded = true;
                self.last_error = Some(format!(
                    "Failed to resolve settings path, using in-memory settings: {err}"
                ));
            }
        }
    }

    fn ensure_runtime_len(&mut self) {
        self.settings.sync_with_fans(&self.runtime.fans);
        if self.hysteresis.len() < self.settings.fans.len() {
            self.hysteresis
                .resize(self.settings.fans.len(), HysteresisState::default());
        }
        if self.fan_previews.len() < self.settings.fans.len() {
            self.fan_previews
                .resize(self.settings.fans.len(), FanPreview::default());
        }
    }

    fn save_settings(&mut self) {
        let Some(path) = self.settings_path.clone() else {
            self.last_error = Some("Settings path is not available".to_owned());
            return;
        };

        self.settings.sync_with_fans(&self.runtime.fans);
        match self.settings.save(&path) {
            Ok(()) => {
                self.settings_dirty = false;
                self.last_status = Some(format!("Saved settings to {}", path.display()));
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(format!("Failed to save settings: {err}"));
            }
        }
    }

    fn reload_settings(&mut self) {
        let Some(path) = self.settings_path.clone() else {
            self.last_error = Some("Settings path is not available".to_owned());
            return;
        };
        match AppSettings::load_or_default(&path, &self.runtime.fans) {
            Ok(settings) => {
                self.settings = settings;
                self.hysteresis = vec![HysteresisState::default(); self.settings.fans.len()];
                self.settings_dirty = false;
                self.last_status = Some(format!("Reloaded settings from {}", path.display()));
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(format!("Failed to reload settings: {err}"));
            }
        }
    }

    fn mark_settings_changed(&mut self, apply_immediately: bool) {
        self.settings_dirty = true;
        if apply_immediately {
            self.pending_immediate_refresh = true;
        }
    }

    fn current_hotspot(&self) -> Option<f64> {
        self.runtime
            .sensors
            .iter()
            .map(|snapshot| snapshot.temp_c)
            .reduce(f64::max)
    }

    fn enable_helper_control(&mut self) {
        match self.run_helper_action(HelperAction::Install) {
            Ok(message) => {
                self.last_status = Some(message);
                self.last_error = None;
                self.refresh_runtime();
            }
            Err(err) => {
                self.last_error = Some(format!("Failed to enable fan control: {err}"));
            }
        }
    }

    fn remove_helper_control(&mut self) {
        match self.run_helper_action(HelperAction::Uninstall) {
            Ok(message) => {
                self.last_status = Some(message);
                self.last_error = None;
                self.refresh_runtime();
            }
            Err(err) => {
                self.last_error = Some(format!("Failed to remove helper: {err}"));
            }
        }
    }

    fn run_helper_action(&self, action: HelperAction) -> anyhow::Result<String> {
        if matches!(action, HelperAction::Install) {
            self.ensure_helper_binary_available()?;
        }

        if is_root() {
            match action {
                HelperAction::Install => install_helper(None)?,
                HelperAction::Uninstall => uninstall_helper()?,
            }

            let status = helper_install_status().unwrap_or_else(|err| format!("unknown ({err})"));
            return Ok(match action {
                HelperAction::Install => format!("Helper installed. {status}"),
                HelperAction::Uninstall => "Helper removed".to_owned(),
            });
        }

        let flag = match action {
            HelperAction::Install => "--install-helper",
            HelperAction::Uninstall => "--uninstall-helper",
        };
        let output = run_privileged_self_command(flag)?;
        let trimmed = output.trim();
        if trimmed.is_empty() {
            Ok(match action {
                HelperAction::Install => "Helper installed".to_owned(),
                HelperAction::Uninstall => "Helper removed".to_owned(),
            })
        } else {
            Ok(trimmed.to_owned())
        }
    }

    fn ensure_helper_binary_available(&self) -> anyhow::Result<()> {
        let current_exe =
            std::env::current_exe().context("failed to locate the current application binary")?;
        let helper_binary = current_exe.with_file_name("apple-silicon-fan-control-helper");
        if helper_binary.exists() {
            Ok(())
        } else {
            anyhow::bail!(
                "helper binary `{}` is missing; rebuild with `task dev` or `cargo build --bins` first",
                helper_binary.display()
            );
        }
    }

    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show_inside(ui, |ui| {
            card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Apple Silicon Fan Control")
                                .font(FontId::proportional(24.0))
                                .strong(),
                        );
                        if let Some(system) = &self.system {
                            ui.label(
                                RichText::new(format!(
                                    "{}  |  {}  |  macOS {} ({})",
                                    system.model_name.as_deref().unwrap_or("Mac"),
                                    system.model_identifier,
                                    system.macos_version,
                                    system.macos_build
                                ))
                                .color(ui.visuals().weak_text_color()),
                            );
                        } else {
                            ui.label(
                                RichText::new("Detecting hardware and macOS version...")
                                    .color(ui.visuals().weak_text_color()),
                            );
                        }
                    });

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let component_button = if self.show_sensor_panel {
                            "Hide Components"
                        } else {
                            "Components"
                        };
                        if ui
                            .add_sized([150.0, 34.0], Button::new(component_button))
                            .clicked()
                        {
                            self.show_sensor_panel = !self.show_sensor_panel;
                        }
                        if ui
                            .add_enabled(
                                self.settings_dirty,
                                Button::new("Save")
                                    .fill(accent_blue())
                                    .min_size(egui::vec2(120.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.save_settings();
                        }
                    });
                });

                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    render_chip(
                        ui,
                        if self.runtime.live_write_available {
                            "Fan control ready"
                        } else {
                            "Fan control locked"
                        },
                        if self.runtime.live_write_available {
                            success_tint()
                        } else {
                            warning_tint()
                        },
                        Color32::WHITE,
                    );
                    if self.settings_dirty {
                        render_chip(ui, "Unsaved", warning_tint(), Color32::WHITE);
                    }
                    if let Some(hotspot) = self.current_hotspot() {
                        render_chip(
                            ui,
                            format!("Hotspot {:.1} C", hotspot),
                            temperature_color(hotspot),
                            Color32::WHITE,
                        );
                    }
                });

                if !self.runtime.live_write_available {
                    render_callout(
                        ui,
                        "You can edit curves here, but real RPM changes will not apply until the privileged helper is installed or the app is started as root.",
                        CalloutTone::Warning,
                    );
                    ui.horizontal_wrapped(|ui| {
                        let action_label = if self.runtime.helper_status.is_installed() {
                            "Repair Helper"
                        } else {
                            "Enable Control"
                        };
                        if ui
                            .add_sized([150.0, 34.0], Button::new(action_label).fill(accent_blue()))
                            .clicked()
                        {
                            self.enable_helper_control();
                        }
                        if self.runtime.helper_status.is_installed()
                            && ui.add_sized([130.0, 34.0], Button::new("Remove Helper")).clicked()
                        {
                            self.remove_helper_control();
                        }
                        ui.label(
                            RichText::new(self.runtime.helper_status.summary())
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                }
                if let Some(error) = &self.last_error {
                    render_callout(ui, error, CalloutTone::Error);
                }

                egui::CollapsingHeader::new("Advanced")
                    .id_salt("top_bar_advanced")
                    .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let refresh = &mut self.settings.refresh_interval_ms;
                        if ui
                            .add_sized(
                                [240.0, 0.0],
                                egui::Slider::new(refresh, 250..=5000).text("Refresh interval"),
                            )
                            .changed()
                        {
                            self.settings_dirty = true;
                        }
                        if ui.button("Refresh").clicked() {
                            self.refresh_runtime();
                        }
                        if ui.button("Reload").clicked() {
                            self.reload_settings();
                        }
                    });

                    ui.label(
                        RichText::new(format!(
                            "Backend: {}",
                            self.runtime.control_backend_label
                        ))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Helper: {}",
                            self.runtime.helper_status.summary()
                        ))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                    if let Some(path) = &self.settings_path {
                        ui.label(
                            RichText::new(
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("settings.yaml"),
                            )
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        )
                        .on_hover_text(path.display().to_string());
                    }
                    if let Some(status) = &self.last_status {
                        ui.label(
                            RichText::new(status)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            });
        });
    }

    fn render_sensor_panel(&mut self, ui: &mut egui::Ui) {
        if !self.show_sensor_panel {
            return;
        }

        egui::Panel::right("sensor_panel")
            .resizable(true)
            .default_size(300.0)
            .show_inside(ui, |ui| {
                card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Components")
                                .font(FontId::proportional(20.0))
                                .strong(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if let Some(hotspot) = self.current_hotspot() {
                                render_chip(
                                    ui,
                                    format!("{:.1} C", hotspot),
                                    temperature_color(hotspot),
                                    Color32::WHITE,
                                );
                            }
                        });
                    });

                    if let Some(profile) = self.profile {
                        ui.label(
                            RichText::new(profile.title).color(ui.visuals().weak_text_color()),
                        );
                    } else {
                        render_callout(
                            ui,
                            "Built-in component labels are not available for this Mac yet.",
                            CalloutTone::Warning,
                        );
                    }

                    ui.add_space(8.0);
                    let filter_width = finite_ui_width(ui, 260.0);
                    ui.add_sized(
                        [filter_width, 0.0],
                        egui::TextEdit::singleline(&mut self.sensor_filter)
                            .hint_text("Filter by label or SMC key"),
                    );

                    ui.add_space(8.0);
                    let filter_query = self.sensor_filter.trim().to_ascii_lowercase();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut visible = 0_usize;
                            for snapshot in self.runtime.sensors.iter().filter(|snapshot| {
                                filter_query.is_empty()
                                    || snapshot.label.to_ascii_lowercase().contains(&filter_query)
                                    || snapshot.key.to_ascii_lowercase().contains(&filter_query)
                            }) {
                                visible += 1;
                                Frame::new()
                                    .fill(surface_subtle())
                                    .stroke(Stroke::new(1.0, outline_color()))
                                    .corner_radius(12)
                                    .inner_margin(6)
                                    .show(ui, |ui| {
                                        egui::CollapsingHeader::new(
                                            RichText::new(format!(
                                                "{}  |  {:.1} C",
                                                display_sensor_label(&snapshot.label),
                                                snapshot.temp_c
                                            ))
                                            .strong()
                                            .color(temperature_color(snapshot.temp_c)),
                                        )
                                        .id_salt(format!("sensor_panel_{}", snapshot.key))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(format!("SMC key: {}", snapshot.key))
                                                    .monospace()
                                                    .small()
                                                    .color(ui.visuals().weak_text_color()),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "Current temperature: {:.1} C",
                                                    snapshot.temp_c
                                                ))
                                                .strong(),
                                            );
                                        });
                                    });
                                ui.add_space(6.0);
                            }

                            if visible == 0 {
                                render_callout(
                                    ui,
                                    "No sensors match the current filter.",
                                    CalloutTone::Info,
                                );
                            }
                        });
                });
            });
    }

    fn render_fans(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.settings.fans.is_empty() {
                        card_frame().show(ui, |ui| {
                            render_callout(ui, "No fans detected yet.", CalloutTone::Info);
                        });
                        return;
                    }

                    for index in 0..self.settings.fans.len() {
                        let fan_info = self.runtime.fans.get(index).cloned();
                        let preview = self.fan_previews.get(index).cloned().unwrap_or_default();
                        self.render_single_fan(ui, index, fan_info.as_ref(), &preview);
                        ui.add_space(12.0);
                    }
                });
        });
    }

    fn render_single_fan(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        fan_info: Option<&FanInfo>,
        preview: &FanPreview,
    ) {
        let profile = self.profile;
        let mut apply_immediately = false;

        {
            let fan_settings = &mut self.settings.fans[index];
            if !matches!(
                fan_settings.mode,
                FanControlMode::Adaptive { .. } | FanControlMode::Auto | FanControlMode::Max
            ) {
                if let Some(fan_info) = fan_info {
                    fan_settings.set_adaptive_default(fan_info);
                } else {
                    fan_settings.mode = FanControlMode::default();
                }
                apply_immediately = true;
            }
            let current_mode = fan_ui_mode_from_settings(&fan_settings.mode);
            let mut selected_mode = current_mode;
            let header_text = match fan_info {
                Some(fan) => format!(
                    "{}  |  {:.0} RPM now  |  {}",
                    fan_settings.label,
                    fan.actual_rpm,
                    fan.mode_label()
                ),
                None => fan_settings.label.clone(),
            };

            card_frame().show(ui, |ui| {
                egui::CollapsingHeader::new(
                    RichText::new(header_text)
                        .font(FontId::proportional(20.0))
                        .strong(),
                )
                .id_salt(format!("fan_card_{index}"))
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        render_fan_mode_selector(ui, &mut selected_mode);
                        if let Some(fan) = fan_info {
                            ui.label(
                                RichText::new(format!(
                                    "Allowed range {:.0}-{:.0} RPM",
                                    fan.min_rpm, fan.max_rpm
                                ))
                                .color(ui.visuals().weak_text_color()),
                            );
                        }
                    });

                    if current_mode != selected_mode {
                        fan_settings.mode = match selected_mode {
                            FanUiMode::Adaptive => {
                                if let Some(fan_info) = fan_info {
                                    crate::app_settings::suggested_adaptive_mode(fan_info)
                                } else {
                                    FanControlMode::default()
                                }
                            }
                            FanUiMode::System => FanControlMode::Auto,
                            FanUiMode::MaxAlways => FanControlMode::Max,
                        };
                        apply_immediately = true;
                    }

                    ui.add_space(10.0);

                    match &mut fan_settings.mode {
                        FanControlMode::Adaptive {
                            target,
                            max_temp_c,
                            hysteresis_c,
                        } => {
                            subtle_frame().show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    render_metric_card(
                                        ui,
                                        "Current Temperature",
                                        preview
                                            .target_temp_c
                                            .map(|temp| format!("{temp:.1} C"))
                                            .unwrap_or_else(|| "--".to_owned()),
                                        preview
                                            .target_temp_c
                                            .map(temperature_color)
                                            .unwrap_or_else(accent_blue),
                                    );
                                    render_metric_card(
                                        ui,
                                        "Target RPM",
                                        preview
                                            .requested_rpm
                                            .map(|rpm| rpm.to_string())
                                            .unwrap_or_else(|| "--".to_owned()),
                                        accent_teal(),
                                    );
                                });

                                ui.add_space(10.0);
                                render_adaptive_controls(
                                    ui,
                                    index,
                                    profile,
                                    fan_info,
                                    target,
                                    max_temp_c,
                                    hysteresis_c,
                                    preview,
                                    &mut apply_immediately,
                                );
                            });
                        }
                        FanControlMode::Auto => {
                            render_callout(
                                ui,
                                "System mode. macOS controls this fan with its default thermal policy.",
                                CalloutTone::Info,
                            );
                        }
                        FanControlMode::Max => {
                            render_callout(
                                ui,
                                "Maximum mode. This fan is forced to its highest allowed RPM all the time.",
                                CalloutTone::Warning,
                            );
                        }
                        _ => {}
                    }

                    if let Some(error) = &preview.error {
                        ui.add_space(10.0);
                        render_callout(ui, error, CalloutTone::Error);
                    }

                    if !preview.sample_details.is_empty() {
                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("Sensor samples used for preview")
                            .id_salt(format!("sensor_samples_{index}"))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&preview.sample_details)
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            });
                    }
                });
            });
        }

        if apply_immediately {
            self.mark_settings_changed(true);
        }
    }
}

impl eframe::App for FanControlApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.instance_guard.take_pending_activations() > 0 {
            self.activate_existing_window(ctx);
        }

        let should_refresh = match self.runtime.last_refresh {
            Some(last_refresh) => {
                last_refresh.elapsed()
                    >= Duration::from_millis(self.settings.refresh_interval_ms.max(250))
            }
            None => true,
        };

        if should_refresh {
            self.refresh_runtime();
        }

        ctx.request_repaint_after(Duration::from_millis(
            self.settings.refresh_interval_ms.max(250),
        ));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render_top_bar(ui);
        self.render_sensor_panel(ui);
        self.render_fans(ui);

        if self.pending_immediate_refresh {
            self.pending_immediate_refresh = false;
            self.refresh_runtime();
            ui.ctx().request_repaint();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CalloutTone {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy)]
enum HelperAction {
    Install,
    Uninstall,
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(Color32::from_rgb(236, 240, 248));
    visuals.panel_fill = Color32::from_rgb(10, 14, 20);
    visuals.window_fill = Color32::from_rgb(14, 19, 28);
    visuals.extreme_bg_color = Color32::from_rgb(8, 11, 17);
    visuals.faint_bg_color = Color32::from_rgb(19, 24, 34);
    visuals.window_corner_radius = CornerRadius::same(18);
    visuals.menu_corner_radius = CornerRadius::same(14);
    visuals.widgets.noninteractive.bg_fill = surface_card();
    visuals.widgets.noninteractive.weak_bg_fill = surface_card();
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, outline_color());
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(12);
    visuals.widgets.inactive.bg_fill = surface_highlight();
    visuals.widgets.inactive.weak_bg_fill = surface_highlight();
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, outline_color());
    visuals.widgets.inactive.corner_radius = CornerRadius::same(12);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(36, 46, 64);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(36, 46, 64);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent_blue());
    visuals.widgets.hovered.corner_radius = CornerRadius::same(12);
    visuals.widgets.active.bg_fill = Color32::from_rgb(53, 82, 137);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(53, 82, 137);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, accent_blue());
    visuals.widgets.active.corner_radius = CornerRadius::same(12);
    visuals.selection.bg_fill = Color32::from_rgb(63, 95, 156);
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(189, 215, 255));
    ctx.set_visuals(visuals);

    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(12.0, 12.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size = egui::vec2(44.0, 32.0);
        style.spacing.slider_width = 220.0;

        if let Some(text) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
            text.size = 24.0;
        }
        if let Some(text) = style.text_styles.get_mut(&egui::TextStyle::Body) {
            text.size = 14.0;
        }
        if let Some(text) = style.text_styles.get_mut(&egui::TextStyle::Button) {
            text.size = 14.0;
        }
        if let Some(text) = style.text_styles.get_mut(&egui::TextStyle::Small) {
            text.size = 12.0;
        }
    });
}

fn card_frame() -> Frame {
    Frame::new()
        .fill(surface_card())
        .stroke(Stroke::new(1.0, outline_color()))
        .corner_radius(18)
        .inner_margin(18)
}

fn subtle_frame() -> Frame {
    Frame::new()
        .fill(surface_subtle())
        .stroke(Stroke::new(1.0, Color32::from_rgb(41, 50, 63)))
        .corner_radius(14)
        .inner_margin(14)
}

fn accent_blue() -> Color32 {
    Color32::from_rgb(99, 163, 255)
}

fn accent_teal() -> Color32 {
    Color32::from_rgb(76, 207, 176)
}

fn accent_amber() -> Color32 {
    Color32::from_rgb(240, 189, 91)
}

fn accent_red() -> Color32 {
    Color32::from_rgb(255, 124, 132)
}

fn surface_card() -> Color32 {
    Color32::from_rgb(18, 24, 35)
}

fn surface_subtle() -> Color32 {
    Color32::from_rgb(13, 18, 27)
}

fn surface_highlight() -> Color32 {
    Color32::from_rgb(28, 36, 49)
}

fn outline_color() -> Color32 {
    Color32::from_rgb(52, 63, 79)
}

fn success_tint() -> Color32 {
    Color32::from_rgb(27, 90, 71)
}

fn warning_tint() -> Color32 {
    Color32::from_rgb(96, 69, 24)
}

fn temperature_color(temp_c: f64) -> Color32 {
    if temp_c < 55.0 {
        accent_teal()
    } else if temp_c < 75.0 {
        accent_amber()
    } else {
        accent_red()
    }
}

fn render_chip(ui: &mut egui::Ui, text: impl Into<String>, fill: Color32, text_color: Color32) {
    Frame::new()
        .fill(fill)
        .corner_radius(12)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.label(
                RichText::new(text.into())
                    .small()
                    .strong()
                    .color(text_color),
            );
        });
}

fn render_metric_card(ui: &mut egui::Ui, label: &str, value: String, accent: Color32) {
    Frame::new()
        .fill(surface_highlight())
        .stroke(Stroke::new(1.0, outline_color()))
        .corner_radius(14)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_width(180.0);
            ui.label(
                RichText::new(label)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new(value)
                    .font(FontId::proportional(24.0))
                    .strong()
                    .color(accent),
            );
        });
}

fn render_callout(ui: &mut egui::Ui, text: &str, tone: CalloutTone) {
    let (fill, stroke, text_color) = match tone {
        CalloutTone::Info => (
            Color32::from_rgb(21, 33, 52),
            Color32::from_rgb(64, 107, 186),
            Color32::from_rgb(186, 216, 255),
        ),
        CalloutTone::Warning => (
            Color32::from_rgb(53, 39, 15),
            Color32::from_rgb(168, 124, 38),
            Color32::from_rgb(255, 227, 171),
        ),
        CalloutTone::Error => (
            Color32::from_rgb(56, 20, 24),
            Color32::from_rgb(179, 74, 86),
            Color32::from_rgb(255, 199, 206),
        ),
    };

    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(text_color));
        });
}

fn finite_ui_width(ui: &egui::Ui, fallback: f32) -> f32 {
    [
        ui.available_width(),
        ui.available_size_before_wrap().x,
        ui.max_rect().width(),
        ui.clip_rect().width(),
    ]
    .into_iter()
    .find(|width| width.is_finite() && *width > 1.0)
    .unwrap_or(fallback)
}

fn run_privileged_self_command(flag: &str) -> anyhow::Result<String> {
    let current_exe =
        std::env::current_exe().context("failed to locate the current application binary")?;
    let shell_command = format!(
        "{} {flag}",
        shell_quote(current_exe.to_string_lossy().as_ref())
    );
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        applescript_escape(&shell_command)
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("failed to start privileged helper action")?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !stdout.is_empty() {
            Ok(stdout)
        } else if !stderr.is_empty() {
            Ok(stderr)
        } else {
            Ok(String::new())
        }
    } else {
        anyhow::bail!(
            "macOS privilege prompt failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn applescript_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn display_sensor_label(label: &str) -> String {
    if let Some(index) = label.strip_prefix("p_core_") {
        return format!("P-core {index}");
    }
    if let Some(index) = label.strip_prefix("e_core_") {
        return format!("E-core {index}");
    }
    label.replace('_', " ")
}

fn group_label(group: &str) -> &'static str {
    match group {
        "all_cpu_candidates" => "Hottest CPU core",
        "cpu_p_candidates" => "Hottest P-core",
        "cpu_e_candidates" => "Hottest E-core",
        _ => "Sensor group",
    }
}

fn target_selection_token(target: &TargetSelector) -> String {
    if let Some(group) = &target.group {
        format!("group:{group}")
    } else if let Some(sensor) = &target.sensor {
        format!("sensor:{sensor}")
    } else {
        "group:all_cpu_candidates".to_owned()
    }
}

fn target_selection_label(target: &TargetSelector, profile: Option<&SensorProfile>) -> String {
    if let Some(group) = &target.group {
        group_label(group).to_owned()
    } else if let Some(sensor) = &target.sensor {
        if let Some(profile) = profile {
            if let Some(sensor_definition) = profile.find_sensor(sensor) {
                return format!(
                    "{} ({})",
                    display_sensor_label(sensor_definition.label),
                    sensor_definition.key
                );
            }
        }
        sensor.clone()
    } else {
        "Select component".to_owned()
    }
}

fn target_selection_options(
    profile: Option<&SensorProfile>,
    target: &TargetSelector,
) -> Vec<(String, String)> {
    let mut options = vec![
        (
            "group:all_cpu_candidates".to_owned(),
            group_label("all_cpu_candidates").to_owned(),
        ),
        (
            "group:cpu_p_candidates".to_owned(),
            group_label("cpu_p_candidates").to_owned(),
        ),
        (
            "group:cpu_e_candidates".to_owned(),
            group_label("cpu_e_candidates").to_owned(),
        ),
    ];

    if let Some(profile) = profile {
        for sensor in profile.sensors {
            options.push((
                format!("sensor:{}", sensor.label),
                format!("{} ({})", display_sensor_label(sensor.label), sensor.key),
            ));
        }
    }

    if let Some(sensor) = &target.sensor {
        let token = format!("sensor:{sensor}");
        if !options.iter().any(|(value, _)| value == &token) {
            options.push((token, format!("Custom sensor ({sensor})")));
        }
    }

    options
}

fn apply_target_selection(target: &mut TargetSelector, selection: &str) {
    if let Some(group) = selection.strip_prefix("group:") {
        target.group = Some(group.to_owned());
        target.sensor = None;
        target.reduce = ReduceOp::Max;
    } else if let Some(sensor) = selection.strip_prefix("sensor:") {
        target.sensor = Some(sensor.to_owned());
        target.group = None;
    }
}

fn fan_ui_mode_from_settings(mode: &FanControlMode) -> FanUiMode {
    match mode {
        FanControlMode::Adaptive { .. } => FanUiMode::Adaptive,
        FanControlMode::Auto => FanUiMode::System,
        FanControlMode::Max => FanUiMode::MaxAlways,
        _ => FanUiMode::Adaptive,
    }
}

fn fan_ui_mode_label(mode: FanUiMode) -> &'static str {
    match mode {
        FanUiMode::Adaptive => "Adaptive",
        FanUiMode::System => "System",
        FanUiMode::MaxAlways => "Max",
    }
}

fn render_fan_mode_selector(ui: &mut egui::Ui, selected: &mut FanUiMode) {
    Frame::new()
        .fill(surface_highlight())
        .corner_radius(14)
        .inner_margin(4)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for mode in [FanUiMode::Adaptive, FanUiMode::System, FanUiMode::MaxAlways] {
                    let is_selected = *selected == mode;
                    let mut button = Button::selectable(is_selected, fan_ui_mode_label(mode))
                        .corner_radius(10)
                        .min_size(egui::vec2(88.0, 32.0));
                    if is_selected {
                        button = button.fill(match mode {
                            FanUiMode::Adaptive => accent_teal(),
                            FanUiMode::System => accent_blue(),
                            FanUiMode::MaxAlways => accent_red(),
                        });
                    }
                    if ui.add(button).clicked() {
                        *selected = mode;
                    }
                }
            });
        });
}

fn render_adaptive_controls(
    ui: &mut egui::Ui,
    index: usize,
    profile: Option<&'static SensorProfile>,
    fan_info: Option<&FanInfo>,
    target: &mut crate::config::TargetSelector,
    max_temp_c: &mut f64,
    hysteresis_c: &mut f64,
    preview: &FanPreview,
    apply_immediately: &mut bool,
) {
    let points = adaptive_curve_points_for_fan(fan_info, *max_temp_c);
    render_adaptive_editor_panel(
        ui,
        index,
        profile,
        target,
        max_temp_c,
        hysteresis_c,
        apply_immediately,
    );
    ui.add_space(10.0);
    render_curve_preview_panel(ui, &points, fan_info, preview);
}

fn render_curve_preview_panel(
    ui: &mut egui::Ui,
    points: &[CurvePoint],
    fan_info: Option<&FanInfo>,
    preview: &FanPreview,
) {
    subtle_frame().show(ui, |ui| {
        let start_temp = points.first().map(|point| point.temp_c);
        let end_temp = points.last().map(|point| point.temp_c);
        let min_rpm = points.iter().map(|point| point.rpm).min();
        let max_rpm = points.iter().map(|point| point.rpm).max();

        ui.label(RichText::new("Adaptive Preview").strong());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} curve points", points.len()))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            if let (Some(start_temp), Some(end_temp), Some(min_rpm), Some(max_rpm)) =
                (start_temp, end_temp, min_rpm, max_rpm)
            {
                render_chip(
                    ui,
                    format!("{start_temp:.0} -> {end_temp:.0} C"),
                    surface_highlight(),
                    ui.visuals().text_color(),
                );
                render_chip(
                    ui,
                    format!("{min_rpm} -> {max_rpm} RPM"),
                    surface_highlight(),
                    ui.visuals().text_color(),
                );
            }
        });
        ui.add_space(8.0);
        draw_curve_plot(
            ui,
            points,
            fan_info,
            preview.target_temp_c,
            preview.requested_rpm,
        );
    });
}

fn adaptive_curve_points_for_fan(fan_info: Option<&FanInfo>, max_temp_c: f64) -> Vec<CurvePoint> {
    let max_temp_c = max_temp_c.max(50.0);
    let floor_temp_c = adaptive_floor_temp_c(max_temp_c);
    let min_rpm = fan_info
        .map(|fan| fan.min_rpm.max(1.0).round() as u32)
        .unwrap_or(1200);
    let max_rpm = fan_info
        .map(|fan| fan.max_rpm.max(fan.min_rpm).round() as u32)
        .unwrap_or(6000);
    let middle_temp_c = floor_temp_c + ((max_temp_c - floor_temp_c) * 0.5);
    let middle_rpm = min_rpm + ((max_rpm.saturating_sub(min_rpm) as f64 * 0.45).round() as u32);

    vec![
        CurvePoint {
            temp_c: floor_temp_c,
            rpm: min_rpm,
        },
        CurvePoint {
            temp_c: middle_temp_c,
            rpm: middle_rpm,
        },
        CurvePoint {
            temp_c: max_temp_c,
            rpm: max_rpm,
        },
    ]
}

fn adaptive_floor_temp_c(max_temp_c: f64) -> f64 {
    (max_temp_c - 25.0).clamp(35.0, max_temp_c - 5.0)
}

fn render_adaptive_editor_panel(
    ui: &mut egui::Ui,
    index: usize,
    profile: Option<&'static SensorProfile>,
    target: &mut crate::config::TargetSelector,
    max_temp_c: &mut f64,
    hysteresis_c: &mut f64,
    apply_immediately: &mut bool,
) {
    subtle_frame().show(ui, |ui| {
        ui.label(RichText::new("Adaptive Mode").strong());
        ui.add_space(8.0);

        ui.label(RichText::new("Component").small().strong());
        let current_token = target_selection_token(target);
        let mut selected_token = current_token.clone();
        let options = target_selection_options(profile, target);
        if !options.is_empty() {
            egui::ComboBox::from_id_salt(format!("component_{index}"))
                .selected_text(target_selection_label(target, profile))
                .show_ui(ui, |ui| {
                    for (token, label) in &options {
                        if ui
                            .selectable_label(selected_token == *token, label)
                            .clicked()
                        {
                            selected_token = token.clone();
                        }
                    }
                });

            if selected_token != current_token {
                apply_target_selection(target, &selected_token);
                *apply_immediately = true;
            }
        } else {
            let sensor = target.sensor.get_or_insert_with(|| "Tf04".to_owned());
            if ui
                .add_sized(
                    [finite_ui_width(ui, 280.0), 0.0],
                    egui::TextEdit::singleline(sensor),
                )
                .changed()
            {
                *apply_immediately = true;
            }
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Maximum cooling temperature").small().strong());
        if ui
            .add_sized(
                [finite_ui_width(ui, 280.0), 0.0],
                egui::Slider::new(max_temp_c, 50.0..=100.0)
                    .text("Max cooling at")
                    .suffix(" C"),
            )
            .changed()
        {
            *apply_immediately = true;
        }
        let floor_temp_c = adaptive_floor_temp_c(*max_temp_c);
        ui.label(
            RichText::new(format!(
                "Below about {:.0} C the fan returns toward its quiet minimum. From {:.0} C to {:.0} C it ramps smoothly up to maximum.",
                floor_temp_c, floor_temp_c, *max_temp_c
            ))
                .small()
                .color(ui.visuals().weak_text_color()),
        );

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Advanced")
            .id_salt(format!("adaptive_advanced_{index}"))
            .show(ui, |ui| {
            if target.group.is_some() {
                egui::ComboBox::from_id_salt(format!("reduce_{index}"))
                    .selected_text(target.reduce.label())
                    .show_ui(ui, |ui| {
                        for reduce in [ReduceOp::Min, ReduceOp::Max, ReduceOp::Average] {
                            if ui
                                .selectable_label(target.reduce == reduce, reduce.label())
                                .clicked()
                            {
                                target.reduce = reduce;
                                *apply_immediately = true;
                            }
                        }
                    });
            }

            if let Some(sensor) = &mut target.sensor {
                let is_custom = profile
                    .map(|profile| profile.find_sensor(sensor).is_none())
                    .unwrap_or(true);
                if is_custom {
                    ui.label(RichText::new("Custom sensor key").small().strong());
                    if ui
                        .add_sized(
                            [finite_ui_width(ui, 280.0), 0.0],
                            egui::TextEdit::singleline(sensor),
                        )
                        .changed()
                    {
                        *apply_immediately = true;
                    }
                }
            }

            if ui
                .add_sized(
                    [finite_ui_width(ui, 280.0), 0.0],
                    egui::Slider::new(hysteresis_c, 0.0..=10.0)
                        .text("Hysteresis")
                        .suffix(" C"),
                )
                .changed()
            {
                *apply_immediately = true;
            }
        });
    });
}

fn draw_curve_plot(
    ui: &mut egui::Ui,
    points: &[CurvePoint],
    fan_info: Option<&FanInfo>,
    current_temp_c: Option<f64>,
    current_rpm: Option<u32>,
) {
    let desired_size = egui::vec2(finite_ui_width(ui, 360.0).max(220.0), 240.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 14, surface_card());
    painter.rect_stroke(
        rect,
        14,
        Stroke::new(1.0, outline_color()),
        StrokeKind::Inside,
    );

    if points.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No curve points",
            FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
        return;
    }

    let x_min = points
        .first()
        .map(|point| point.temp_c.floor() - 5.0)
        .unwrap_or(40.0);
    let mut x_max = points
        .last()
        .map(|point| point.temp_c.ceil() + 5.0)
        .unwrap_or(90.0);
    if x_max <= x_min {
        x_max = x_min + 10.0;
    }

    let mut y_min = fan_info
        .map(|fan| fan.min_rpm.max(1.0) as f32)
        .unwrap_or_else(|| {
            points
                .iter()
                .map(|point| point.rpm as f32)
                .fold(f32::INFINITY, f32::min)
        });
    if !y_min.is_finite() {
        y_min = 0.0;
    }

    let mut y_max = fan_info.map(|fan| fan.max_rpm as f32).unwrap_or_else(|| {
        points
            .iter()
            .map(|point| point.rpm as f32)
            .fold(0.0, f32::max)
    });
    if let Some(rpm) = current_rpm {
        y_min = y_min.min(rpm as f32);
        y_max = y_max.max(rpm as f32);
    }
    if y_max <= y_min {
        y_max = y_min + 1000.0;
    }

    let plot_rect = rect.shrink2(egui::vec2(22.0, 24.0));

    for step in 0..=4 {
        let t = step as f32 / 4.0;
        let x = plot_rect.left() + (plot_rect.width() * t);
        let y = plot_rect.bottom() - (plot_rect.height() * t);
        painter.line_segment(
            [
                egui::pos2(plot_rect.left(), y),
                egui::pos2(plot_rect.right(), y),
            ],
            Stroke::new(1.0, Color32::from_rgb(32, 38, 52)),
        );
        painter.line_segment(
            [
                egui::pos2(x, plot_rect.top()),
                egui::pos2(x, plot_rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_rgb(32, 38, 52)),
        );
    }

    let to_screen = |temp_c: f64, rpm: u32| -> egui::Pos2 {
        let x_t = ((temp_c - x_min) / (x_max - x_min)).clamp(0.0, 1.0) as f32;
        let y_t = (((rpm as f32) - y_min) / (y_max - y_min)).clamp(0.0, 1.0);
        egui::pos2(
            plot_rect.left() + (plot_rect.width() * x_t),
            plot_rect.bottom() - (plot_rect.height() * y_t),
        )
    };

    for window in points.windows(2) {
        let left = to_screen(window[0].temp_c, window[0].rpm);
        let right = to_screen(window[1].temp_c, window[1].rpm);
        painter.line_segment([left, right], Stroke::new(2.5, accent_blue()));
    }

    for point in points {
        let pos = to_screen(point.temp_c, point.rpm);
        painter.circle_filled(pos, 5.5, accent_teal());
        painter.circle_stroke(pos, 5.5, Stroke::new(1.5, Color32::from_white_alpha(210)));
    }

    if let Some(temp_c) = current_temp_c {
        if let Some(rpm) = current_rpm.or_else(|| interpolate_curve_points(points, temp_c)) {
            let pos = to_screen(temp_c, rpm);
            painter.line_segment(
                [
                    egui::pos2(pos.x, plot_rect.top()),
                    egui::pos2(pos.x, plot_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_white_alpha(40)),
            );
            painter.line_segment(
                [
                    egui::pos2(plot_rect.left(), pos.y),
                    egui::pos2(plot_rect.right(), pos.y),
                ],
                Stroke::new(1.0, Color32::from_white_alpha(40)),
            );
            painter.circle_filled(pos, 8.0, temperature_color(temp_c));
            painter.circle_stroke(pos, 8.0, Stroke::new(2.0, Color32::WHITE));
        }
    }

    painter.text(
        plot_rect.left_bottom() + egui::vec2(0.0, 10.0),
        Align2::LEFT_TOP,
        format!("{x_min:.0} C"),
        FontId::proportional(13.0),
        Color32::from_rgb(198, 208, 224),
    );
    painter.text(
        plot_rect.right_bottom() + egui::vec2(0.0, 10.0),
        Align2::RIGHT_TOP,
        format!("{x_max:.0} C"),
        FontId::proportional(13.0),
        Color32::from_rgb(198, 208, 224),
    );
    painter.text(
        plot_rect.left_top() + egui::vec2(0.0, -6.0),
        Align2::LEFT_BOTTOM,
        format!("{y_max:.0} RPM"),
        FontId::proportional(13.0),
        Color32::from_rgb(198, 208, 224),
    );
    painter.text(
        plot_rect.left_bottom() + egui::vec2(0.0, 6.0),
        Align2::LEFT_TOP,
        format!("{y_min:.0} RPM"),
        FontId::proportional(13.0),
        Color32::from_rgb(198, 208, 224),
    );
}

fn evaluate_fan_mode(
    smc: &AppleSmc,
    fan_index: usize,
    fan: &FanInfo,
    fan_settings: Option<&crate::app_settings::FanSettings>,
    profile: Option<&'static SensorProfile>,
    hysteresis: Option<&mut HysteresisState>,
) -> (FanPreview, Option<FanControlAction>) {
    let Some(fan_settings) = fan_settings else {
        return (
            FanPreview {
                error: Some("Missing fan settings".to_owned()),
                ..Default::default()
            },
            None,
        );
    };

    match &fan_settings.mode {
        FanControlMode::Adaptive {
            target,
            max_temp_c,
            hysteresis_c,
        } => {
            let mut preview = FanPreview::default();
            let resolved = match resolve_target_sensors(target, profile) {
                Ok(resolved) => resolved,
                Err(err) => {
                    preview.error = Some(err.to_string());
                    return (preview, None);
                }
            };

            let snapshots = read_sensor_snapshots_best_effort(smc, &resolved);
            if snapshots.is_empty() {
                preview.error = Some("No readable sensors for the selected component".to_owned());
                return (preview, None);
            }

            let Some(target_temp_c) = reduce_target_temperature(target, &snapshots) else {
                preview.error = Some("Failed to reduce sensor temperatures".to_owned());
                return (preview, None);
            };

            let points = adaptive_curve_points_for_fan(Some(fan), *max_temp_c);
            let Some(interpolated_rpm) = interpolate_curve_points(&points, target_temp_c) else {
                preview.error = Some("Adaptive curve could not be generated".to_owned());
                return (preview, None);
            };

            let requested_rpm = match hysteresis {
                Some(hysteresis) => {
                    fan.clamp_rpm(hysteresis.apply(target_temp_c, interpolated_rpm, *hysteresis_c))
                }
                None => fan.clamp_rpm(interpolated_rpm),
            };

            preview.target_temp_c = Some(target_temp_c);
            preview.requested_rpm = Some(requested_rpm);
            preview.sample_details = format_snapshots(&snapshots);

            (
                preview,
                Some(FanControlAction::SetTargetRpm {
                    fan_index,
                    rpm: requested_rpm,
                }),
            )
        }
        FanControlMode::Auto => (
            FanPreview::default(),
            Some(FanControlAction::Auto { fan_index }),
        ),
        FanControlMode::Max => {
            let requested_rpm = fan.clamp_rpm(fan.max_rpm.round() as u32);
            (
                FanPreview {
                    requested_rpm: Some(requested_rpm),
                    sample_details: "Always max RPM".to_owned(),
                    ..Default::default()
                },
                Some(FanControlAction::SetTargetRpm {
                    fan_index,
                    rpm: requested_rpm,
                }),
            )
        }
        FanControlMode::Fixed { rpm } => {
            let requested_rpm = fan.clamp_rpm(*rpm);
            (
                FanPreview {
                    requested_rpm: Some(requested_rpm),
                    ..Default::default()
                },
                Some(FanControlAction::SetTargetRpm {
                    fan_index,
                    rpm: requested_rpm,
                }),
            )
        }
        FanControlMode::Curve {
            target,
            hysteresis_c,
            points,
        } => {
            let mut preview = FanPreview::default();

            let resolved = match resolve_target_sensors(target, profile) {
                Ok(resolved) => resolved,
                Err(err) => {
                    preview.error = Some(err.to_string());
                    return (preview, None);
                }
            };

            let snapshots = read_sensor_snapshots_best_effort(smc, &resolved);
            if snapshots.is_empty() {
                preview.error = Some("No readable sensors for the selected target".to_owned());
                return (preview, None);
            }

            let Some(target_temp_c) = reduce_target_temperature(target, &snapshots) else {
                preview.error = Some("Failed to reduce sensor temperatures".to_owned());
                return (preview, None);
            };

            let Some(interpolated_rpm) =
                crate::config::interpolate_curve_points(points, target_temp_c)
            else {
                preview.error = Some("Curve has no valid points".to_owned());
                return (preview, None);
            };

            let requested_rpm = match hysteresis {
                Some(hysteresis) => {
                    fan.clamp_rpm(hysteresis.apply(target_temp_c, interpolated_rpm, *hysteresis_c))
                }
                None => fan.clamp_rpm(interpolated_rpm),
            };

            preview.target_temp_c = Some(target_temp_c);
            preview.requested_rpm = Some(requested_rpm);
            preview.sample_details = format_snapshots(&snapshots);

            (
                preview,
                Some(FanControlAction::SetTargetRpm {
                    fan_index,
                    rpm: requested_rpm,
                }),
            )
        }
    }
}
