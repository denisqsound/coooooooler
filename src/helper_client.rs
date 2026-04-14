use crate::{
    fan_control::FanControlPlan,
    helper_paths::helper_socket_path,
    helper_protocol::{HelperRequest, HelperResponse, HelperStatus},
};
use anyhow::{Context, Result, anyhow, bail};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct HelperClient {
    socket_path: PathBuf,
}

impl HelperClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn system() -> Self {
        Self::new(helper_socket_path())
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub fn is_installed(&self) -> bool {
        self.socket_path.exists()
    }

    pub fn ping(&self) -> Result<HelperStatus> {
        let response = self.send(HelperRequest::Ping)?;
        if !response.ok {
            bail!(response.message);
        }
        response
            .status
            .ok_or_else(|| anyhow!("helper ping succeeded but returned no status"))
    }

    pub fn read_status(&self) -> Result<HelperStatus> {
        let response = self.send(HelperRequest::ReadStatus)?;
        if !response.ok {
            bail!(response.message);
        }
        response
            .status
            .ok_or_else(|| anyhow!("helper returned no status payload"))
    }

    pub fn apply_plan(&self, plan: &FanControlPlan) -> Result<()> {
        let response = self.send(HelperRequest::ApplyPlan { plan: plan.clone() })?;
        if response.ok {
            Ok(())
        } else {
            bail!(response.message)
        }
    }

    fn send(&self, request: HelperRequest) -> Result<HelperResponse> {
        let mut stream = UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "failed to connect to helper socket `{}`",
                self.socket_path.display()
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .context("failed to set helper socket read timeout")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .context("failed to set helper socket write timeout")?;

        let payload = serde_json::to_vec(&request).context("failed to encode helper request")?;
        stream
            .write_all(&payload)
            .context("failed to write helper request payload")?;
        stream
            .write_all(b"\n")
            .context("failed to write helper request terminator")?;
        stream.flush().context("failed to flush helper request")?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("failed to read helper response")?;
        if line.trim().is_empty() {
            bail!("helper returned an empty response");
        }

        serde_json::from_str::<HelperResponse>(line.trim())
            .context("failed to decode helper response")
    }
}
