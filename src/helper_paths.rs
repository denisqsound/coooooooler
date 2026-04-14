use std::path::PathBuf;

pub const HELPER_SERVICE_LABEL: &str = "com.denisqsound.apple-silicon-fan-control.helper";

pub fn helper_install_dir() -> PathBuf {
    PathBuf::from("/Library/Application Support/AppleSiliconFanControl")
}

pub fn helper_socket_path() -> PathBuf {
    PathBuf::from("/var/run/apple-silicon-fan-control-helper.sock")
}

pub fn helper_binary_path() -> PathBuf {
    helper_install_dir().join("apple-silicon-fan-control-helper")
}

pub fn helper_launch_daemon_plist_path() -> PathBuf {
    PathBuf::from(format!(
        "/Library/LaunchDaemons/{HELPER_SERVICE_LABEL}.plist"
    ))
}

pub fn helper_stdout_log_path() -> PathBuf {
    helper_install_dir().join("helper.stdout.log")
}

pub fn helper_stderr_log_path() -> PathBuf {
    helper_install_dir().join("helper.stderr.log")
}
