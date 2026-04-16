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

const HELPER_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const HELPER_APPLY_PLAN_TIMEOUT: Duration = Duration::from_secs(15);

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
        let read_timeout = response_timeout(&request);
        let mut stream = UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "failed to connect to helper socket `{}`",
                self.socket_path.display()
            )
        })?;
        stream
            .set_read_timeout(Some(read_timeout))
            .context("failed to set helper socket read timeout")?;
        stream
            .set_write_timeout(Some(HELPER_REQUEST_TIMEOUT))
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
            .with_context(|| {
                format!(
                    "failed to read helper response within {}s",
                    read_timeout.as_secs()
                )
            })?;
        if line.trim().is_empty() {
            bail!("helper returned an empty response");
        }

        serde_json::from_str::<HelperResponse>(line.trim())
            .context("failed to decode helper response")
    }
}

fn response_timeout(request: &HelperRequest) -> Duration {
    match request {
        // The helper can spend up to 10s unlocking manual fan control before it replies.
        HelperRequest::ApplyPlan { .. } => HELPER_APPLY_PLAN_TIMEOUT,
        HelperRequest::Ping | HelperRequest::ReadStatus => HELPER_REQUEST_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fan_control::FanControlPlan;

    #[test]
    fn ping_uses_short_timeout() {
        assert_eq!(response_timeout(&HelperRequest::Ping), HELPER_REQUEST_TIMEOUT);
    }

    #[test]
    fn apply_plan_uses_extended_timeout() {
        let request = HelperRequest::ApplyPlan {
            plan: FanControlPlan::new(Vec::new()),
        };

        assert_eq!(response_timeout(&request), HELPER_APPLY_PLAN_TIMEOUT);
    }
}
