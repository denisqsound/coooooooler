use crate::{
    fan_control::{FanControlPlan, apply_plan_direct},
    helper_client::HelperClient,
    platform::is_root,
    smc_controller::AppleSmc,
};
use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub enum ControlBackend {
    DirectRoot,
    Helper(HelperClient),
    Unavailable,
}

impl ControlBackend {
    pub fn detect() -> Self {
        if is_root() {
            return Self::DirectRoot;
        }

        let client = HelperClient::system();
        if client.ping().is_ok() {
            Self::Helper(client)
        } else {
            Self::Unavailable
        }
    }

    pub fn can_write(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    pub fn label(&self) -> String {
        match self {
            Self::DirectRoot => "direct root access".to_owned(),
            Self::Helper(client) => {
                format!("privileged helper ({})", client.socket_path().display())
            }
            Self::Unavailable => "read-only (no root/helper)".to_owned(),
        }
    }

    pub fn apply_plan(&self, smc: &AppleSmc, plan: &FanControlPlan) -> Result<()> {
        if plan.is_empty() {
            return Ok(());
        }

        match self {
            Self::DirectRoot => apply_plan_direct(smc, plan),
            Self::Helper(client) => client.apply_plan(plan),
            Self::Unavailable => bail!("no write backend available"),
        }
    }
}
