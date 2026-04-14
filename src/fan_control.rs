use crate::smc_controller::AppleSmc;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanControlPlan {
    pub actions: Vec<FanControlAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FanControlAction {
    Auto { fan_index: usize },
    SetTargetRpm { fan_index: usize, rpm: u32 },
}

impl FanControlPlan {
    pub fn new(actions: Vec<FanControlAction>) -> Self {
        Self { actions }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn normalized_actions(&self) -> Vec<FanControlAction> {
        let mut by_fan = BTreeMap::new();
        for action in &self.actions {
            by_fan.insert(action.fan_index(), action.clone());
        }
        by_fan.into_values().collect()
    }

    pub fn manual_fan_indices(&self) -> Vec<usize> {
        self.normalized_actions()
            .into_iter()
            .filter_map(|action| match action {
                FanControlAction::SetTargetRpm { fan_index, .. } => Some(fan_index),
                FanControlAction::Auto { .. } => None,
            })
            .collect()
    }
}

impl FanControlAction {
    pub fn fan_index(&self) -> usize {
        match *self {
            Self::Auto { fan_index } | Self::SetTargetRpm { fan_index, .. } => fan_index,
        }
    }
}

pub fn apply_plan_direct(smc: &AppleSmc, plan: &FanControlPlan) -> Result<()> {
    let actions = plan.normalized_actions();
    if actions.is_empty() {
        return Ok(());
    }

    let manual_indices: Vec<usize> = actions
        .iter()
        .filter_map(|action| match action {
            FanControlAction::SetTargetRpm { fan_index, .. } => Some(*fan_index),
            FanControlAction::Auto { .. } => None,
        })
        .collect();

    if !manual_indices.is_empty() {
        smc.ensure_manual_control_for_fans(&manual_indices)?;
    }

    for action in &actions {
        match *action {
            FanControlAction::Auto { fan_index } => {
                smc.set_auto_mode_without_releasing_test_mode(fan_index)?;
            }
            FanControlAction::SetTargetRpm { fan_index, rpm } => {
                smc.set_target_rpm(fan_index, rpm)?;
            }
        }
    }

    if manual_indices.is_empty() {
        smc.release_test_mode()?;
    }

    Ok(())
}
