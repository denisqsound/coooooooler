use std::process::ExitCode;

enum ManagementCommand {
    Gui,
    InstallHelper,
    UninstallHelper,
    HelperStatus,
}

fn main() -> ExitCode {
    let command = match parse_command() {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    match command {
        ManagementCommand::Gui => match run_gui() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(1)
            }
        },
        ManagementCommand::InstallHelper => {
            match apple_silicon_fan_control::helper_install::install_helper(None) {
                Ok(()) => {
                    let status = apple_silicon_fan_control::helper_install::helper_install_status()
                        .unwrap_or_else(|err| format!("unknown ({err})"));
                    println!("Helper installed. {status}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(1)
                }
            }
        }
        ManagementCommand::UninstallHelper => {
            match apple_silicon_fan_control::helper_install::uninstall_helper() {
                Ok(()) => {
                    println!("Helper removed");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(1)
                }
            }
        }
        ManagementCommand::HelperStatus => {
            match apple_silicon_fan_control::helper_install::helper_install_status() {
                Ok(status) => {
                    println!("{status}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn parse_command() -> Result<ManagementCommand, String> {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        return Ok(ManagementCommand::Gui);
    };

    if args.next().is_some() {
        return Err(format!("unsupported arguments starting with `{first}`"));
    }

    match first.as_str() {
        "--install-helper" => Ok(ManagementCommand::InstallHelper),
        "--uninstall-helper" => Ok(ManagementCommand::UninstallHelper),
        "--helper-status" => Ok(ManagementCommand::HelperStatus),
        _ => Err(format!("unknown argument `{first}`")),
    }
}

fn run_gui() -> eframe::Result<()> {
    let instance_guard = match apple_silicon_fan_control::single_instance::SingleInstanceGuard::acquire_or_activate_existing() {
        Ok(Some(guard)) => guard,
        Ok(None) => return Ok(()),
        Err(err) => {
            eprintln!("{err}");
            apple_silicon_fan_control::single_instance::show_already_running_notice();
            return Ok(());
        }
    };

    apple_silicon_fan_control::gui::run(instance_guard)
}
