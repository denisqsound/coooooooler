use crate::{
    helper_client::HelperClient,
    helper_paths::{
        HELPER_SERVICE_LABEL, helper_binary_path, helper_install_dir,
        helper_launch_daemon_plist_path, helper_socket_path, helper_stderr_log_path,
        helper_stdout_log_path,
    },
    platform::is_root,
};
use anyhow::{Context, Result, anyhow, bail};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub fn install_helper(source_binary: Option<&Path>) -> Result<()> {
    ensure_root()?;

    let source_binary = resolve_helper_source_binary(source_binary)?;
    let install_dir = helper_install_dir();
    fs::create_dir_all(&install_dir)
        .with_context(|| format!("failed to create `{}`", install_dir.display()))?;

    let installed_binary = helper_binary_path();
    fs::copy(&source_binary, &installed_binary).with_context(|| {
        format!(
            "failed to copy helper binary from `{}` to `{}`",
            source_binary.display(),
            installed_binary.display()
        )
    })?;
    fs::set_permissions(&installed_binary, fs::Permissions::from_mode(0o755)).with_context(
        || {
            format!(
                "failed to make helper binary executable `{}`",
                installed_binary.display()
            )
        },
    )?;

    let plist_path = helper_launch_daemon_plist_path();
    fs::write(
        &plist_path,
        launch_daemon_plist(&installed_binary, &helper_socket_path()),
    )
    .with_context(|| format!("failed to write plist `{}`", plist_path.display()))?;

    let _ = run_launchctl(["bootout", "system", plist_path.to_string_lossy().as_ref()]);
    run_launchctl(["bootstrap", "system", plist_path.to_string_lossy().as_ref()])?;
    run_launchctl(["kickstart", "-k", &format!("system/{HELPER_SERVICE_LABEL}")])?;

    Ok(())
}

pub fn uninstall_helper() -> Result<()> {
    ensure_root()?;

    let plist_path = helper_launch_daemon_plist_path();
    let _ = run_launchctl(["bootout", "system", plist_path.to_string_lossy().as_ref()]);

    for path in [
        helper_socket_path(),
        helper_binary_path(),
        helper_stdout_log_path(),
        helper_stderr_log_path(),
        plist_path,
    ] {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }

    let install_dir = helper_install_dir();
    if install_dir.exists() {
        let _ = fs::remove_dir(&install_dir);
    }

    Ok(())
}

pub fn helper_install_status() -> Result<String> {
    let client = HelperClient::system();
    let installed = helper_binary_path().exists();
    let plist = helper_launch_daemon_plist_path().exists();
    let socket = client.socket_path().exists();

    let helper_state = match client.ping() {
        Ok(status) => format!("running via socket {}", status.socket_path),
        Err(err) => format!("not reachable ({err})"),
    };

    Ok(format!(
        "installed_binary={} plist={} socket={} helper={}",
        bool_word(installed),
        bool_word(plist),
        bool_word(socket),
        helper_state
    ))
}

fn ensure_root() -> Result<()> {
    if is_root() {
        Ok(())
    } else {
        bail!("helper installation requires root privileges")
    }
}

fn resolve_helper_source_binary(source_binary: Option<&Path>) -> Result<PathBuf> {
    if let Some(source_binary) = source_binary {
        if source_binary.exists() {
            return Ok(source_binary.to_path_buf());
        }
        bail!("helper binary `{}` does not exist", source_binary.display());
    }

    let current_exe = std::env::current_exe().context("failed to locate current executable")?;
    let candidate = current_exe.with_file_name("apple-silicon-fan-control-helper");
    if candidate.exists() {
        return Ok(candidate);
    }

    if current_exe
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value == "apple-silicon-fan-control-helper")
        .unwrap_or(false)
    {
        return Ok(current_exe);
    }

    Err(anyhow!(
        "could not find `apple-silicon-fan-control-helper` next to `{}`; pass `--helper-binary` explicitly",
        current_exe.display()
    ))
}

fn run_launchctl<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .context("failed to spawn launchctl")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(anyhow!(
        "launchctl {:?} failed with status {}: {} {}",
        args,
        output.status,
        stdout.trim(),
        stderr.trim()
    ))
}

fn launch_daemon_plist(installed_binary: &Path, socket_path: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>--socket</string>
    <string>{socket}</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout_log}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_log}</string>
</dict>
</plist>
"#,
        label = HELPER_SERVICE_LABEL,
        binary = installed_binary.display(),
        socket = socket_path.display(),
        stdout_log = helper_stdout_log_path().display(),
        stderr_log = helper_stderr_log_path().display(),
    )
}

fn bool_word(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
