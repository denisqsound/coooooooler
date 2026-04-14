use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorKind {
    CpuPerformanceCandidate,
    CpuEfficiencyCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy)]
pub struct SensorDefinition {
    pub label: &'static str,
    pub key: &'static str,
    pub kind: SensorKind,
    pub confidence: Confidence,
}

#[derive(Debug)]
pub struct SensorProfile {
    pub model_identifier: &'static str,
    pub title: &'static str,
    pub sensors: &'static [SensorDefinition],
    pub notes: &'static [&'static str],
}

impl SensorProfile {
    pub fn find_sensor(&self, label_or_key: &str) -> Option<&'static SensorDefinition> {
        self.sensors
            .iter()
            .find(|sensor| sensor.label == label_or_key || sensor.key == label_or_key)
    }

    pub fn sensors_for_group(&self, group: &str) -> Option<Vec<&'static SensorDefinition>> {
        match group {
            "all_cpu_candidates" => Some(self.sensors.iter().collect()),
            "cpu_p_candidates" => Some(
                self.sensors
                    .iter()
                    .filter(|sensor| sensor.kind == SensorKind::CpuPerformanceCandidate)
                    .collect(),
            ),
            "cpu_e_candidates" => Some(
                self.sensors
                    .iter()
                    .filter(|sensor| sensor.kind == SensorKind::CpuEfficiencyCandidate)
                    .collect(),
            ),
            _ => None,
        }
    }

    pub fn supported_groups(&self) -> &'static [&'static str] {
        &["all_cpu_candidates", "cpu_p_candidates", "cpu_e_candidates"]
    }
}

impl fmt::Display for SensorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CpuPerformanceCandidate => write!(f, "P-core candidate"),
            Self::CpuEfficiencyCandidate => write!(f, "E-core candidate"),
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
        }
    }
}

pub fn profile_for_model(model_identifier: &str) -> Option<&'static SensorProfile> {
    match model_identifier {
        "Mac15,10" => Some(&MAC15_10_M3_MAX_14C_PROFILE),
        _ => None,
    }
}

const MAC15_10_M3_MAX_14C_SENSORS: [SensorDefinition; 14] = [
    SensorDefinition {
        label: "e_core_1",
        key: "Te05",
        kind: SensorKind::CpuEfficiencyCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "e_core_2",
        key: "Te0L",
        kind: SensorKind::CpuEfficiencyCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "e_core_3",
        key: "Te0P",
        kind: SensorKind::CpuEfficiencyCandidate,
        confidence: Confidence::Low,
    },
    SensorDefinition {
        label: "e_core_4",
        key: "Te0S",
        kind: SensorKind::CpuEfficiencyCandidate,
        confidence: Confidence::Low,
    },
    SensorDefinition {
        label: "p_core_1",
        key: "Tf04",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_2",
        key: "Tf09",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_3",
        key: "Tf0A",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_4",
        key: "Tf0B",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_5",
        key: "Tf0D",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_6",
        key: "Tf0E",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_7",
        key: "Tf44",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_8",
        key: "Tf49",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_9",
        key: "Tf4A",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
    SensorDefinition {
        label: "p_core_10",
        key: "Tf4B",
        kind: SensorKind::CpuPerformanceCandidate,
        confidence: Confidence::Medium,
    },
];

const MAC15_10_M3_MAX_14C_NOTES: [&str; 3] = [
    "This profile is community-derived for the 14-core M3 Max and should be validated with `probe` on your exact machine.",
    "E-core keys on M3 Max are less certain than P-core keys; low-confidence sensors should be treated as best effort.",
    "If a label looks wrong on your workload, you can target the raw 4-character SMC key directly in the YAML config.",
];

static MAC15_10_M3_MAX_14C_PROFILE: SensorProfile = SensorProfile {
    model_identifier: "Mac15,10",
    title: "MacBookPro20,4 / Apple M3 Max (14-core CPU)",
    sensors: &MAC15_10_M3_MAX_14C_SENSORS,
    notes: &MAC15_10_M3_MAX_14C_NOTES,
};
