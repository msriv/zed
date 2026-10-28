use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

use crate::DockSide;

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct TwgSettingsContent {
    /// Path to the twg executable. Empty discovers twg on PATH and common installation paths.
    pub executable: Option<String>,
    /// Atlassian site passed to twg. Required for Jira ticket requests.
    pub site: Option<String>,
    /// Default Jira project key for the Work panel.
    pub default_project: Option<String>,
    /// Auto refresh interval in minutes. Zero disables automatic refresh.
    pub refresh_interval_minutes: Option<u64>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct WorkPanelSettingsContent {
    pub dock: Option<DockSide>,
    pub default_width: Option<f32>,
    pub button: Option<bool>,
}
