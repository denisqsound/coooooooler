use crate::{
    fan_control::apply_plan_direct,
    helper_paths::helper_socket_path,
    helper_protocol::{HelperRequest, HelperResponse, HelperStatus},
    platform::is_root,
    smc_controller::AppleSmc,
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
};

pub fn serve(socket_path: Option<PathBuf>) -> Result<()> {
    if !is_root() {
        bail!("helper must run as root");
    }

    let socket_path = socket_path.unwrap_or_else(helper_socket_path);
    if socket_path.exists() {
        fs::remove_file(&socket_path).with_context(|| {
            format!(
                "failed to remove stale helper socket `{}`",
                socket_path.display()
            )
        })?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind helper socket `{}`", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666)).with_context(|| {
        format!(
            "failed to set helper socket permissions `{}`",
            socket_path.display()
        )
    })?;

    let smc = AppleSmc::connect().context("failed to connect to AppleSMC in helper")?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let response = match read_request(&mut stream) {
                    Ok(request) => handle_request(&smc, &socket_path, request),
                    Err(err) => HelperResponse::err(err.to_string()),
                };
                let _ = write_response(&mut stream, &response);
            }
            Err(err) => {
                eprintln!("helper accept failed: {err}");
            }
        }
    }

    Ok(())
}

fn read_request(stream: &mut std::os::unix::net::UnixStream) -> Result<HelperRequest> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("failed to read helper request line")?;
    if line.trim().is_empty() {
        bail!("helper request was empty");
    }
    let request =
        serde_json::from_str::<HelperRequest>(line.trim()).context("failed to parse request")?;
    Ok(request)
}

fn write_response(
    stream: &mut std::os::unix::net::UnixStream,
    response: &HelperResponse,
) -> Result<()> {
    let payload = serde_json::to_vec(response).context("failed to encode helper response")?;
    stream
        .write_all(&payload)
        .context("failed to write helper response")?;
    stream
        .write_all(b"\n")
        .context("failed to terminate helper response")?;
    stream.flush().context("failed to flush helper response")
}

fn handle_request(smc: &AppleSmc, socket_path: &PathBuf, request: HelperRequest) -> HelperResponse {
    match request {
        HelperRequest::Ping | HelperRequest::ReadStatus => {
            HelperResponse::ok_with_status("helper is running", helper_status(socket_path))
        }
        HelperRequest::ApplyPlan { plan } => match apply_plan_direct(smc, &plan) {
            Ok(()) => HelperResponse::ok("plan applied"),
            Err(err) => HelperResponse::err(err.to_string()),
        },
    }
}

fn helper_status(socket_path: &PathBuf) -> HelperStatus {
    HelperStatus {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        socket_path: socket_path.display().to_string(),
    }
}
