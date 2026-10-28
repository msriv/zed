use settings::{RegisterSetting, Settings};

#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct TwgSettings {
    pub executable: String,
    pub site: String,
    pub default_project: String,
    pub refresh_interval_minutes: u64,
}

impl Settings for TwgSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let settings = content.twg.clone().unwrap_or_default();
        Self {
            executable: settings.executable.unwrap_or_default(),
            site: settings.site.unwrap_or_default(),
            default_project: settings.default_project.unwrap_or_default(),
            refresh_interval_minutes: settings.refresh_interval_minutes.unwrap_or(5),
        }
    }
}

impl TwgSettings {
    pub fn client(&self) -> crate::Client {
        crate::Client::new(
            crate::discover_executable(Some(&self.executable)),
            (!self.site.trim().is_empty()).then(|| self.site.trim().to_owned()),
        )
    }
}
