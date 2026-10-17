//! Meeting apps the detection knows, and how each OS names them.
//!
//! Unknown apps using the microphone are ignored. Browsers are listed but off
//! by default: a call cannot be told apart from any other use of the
//! microphone in a tab.

use super::MicUser;
use serde::Serialize;
use specta::Type;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Native,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

#[derive(Debug)]
pub struct CatalogEntry {
    /// Stable id, used as the key of `meeting_detection_apps`.
    pub id: &'static str,
    pub display_name: &'static str,
    /// Executable names or package family names, compared ignoring case.
    pub windows: &'static [&'static str],
    /// Bundle ID prefixes (`com.microsoft.teams2` also matches its helpers,
    /// `com.microsoft.teams2.*`).
    pub macos: &'static [&'static str],
    pub kind: AppKind,
    pub default_enabled: bool,
}

impl CatalogEntry {
    fn patterns(&self, platform: Platform) -> &'static [&'static str] {
        match platform {
            Platform::Windows => self.windows,
            Platform::MacOs => self.macos,
            Platform::Other => &[],
        }
    }

    pub fn available_on(&self, platform: Platform) -> bool {
        !self.patterns(platform).is_empty()
    }

    pub fn enabled(&self, overrides: &HashMap<String, bool>) -> bool {
        overrides
            .get(self.id)
            .copied()
            .unwrap_or(self.default_enabled)
    }
}

pub static CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "teams",
        display_name: "Microsoft Teams",
        windows: &["MSTeams_8wekyb3d8bbwe", "ms-teams.exe"],
        macos: &["com.microsoft.teams2"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "teams_classic",
        display_name: "Microsoft Teams (classic)",
        windows: &["teams.exe"],
        macos: &["com.microsoft.teams"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "zoom",
        display_name: "Zoom",
        windows: &["zoom.exe"],
        macos: &["us.zoom.xos"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "slack",
        display_name: "Slack",
        windows: &["slack.exe"],
        macos: &["com.tinyspeck.slackmacgap"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "webex",
        display_name: "Webex",
        windows: &["webex.exe", "ciscocollabhost.exe", "atmgr.exe"],
        macos: &["Cisco-Systems.Spark", "com.webex.meetingmanager"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "browser",
        display_name: "Google Meet / Teams (browser)",
        windows: &[
            "chrome.exe",
            "msedge.exe",
            "firefox.exe",
            "brave.exe",
            "opera.exe",
        ],
        macos: &[
            "com.google.Chrome",
            "com.microsoft.edgemac",
            "org.mozilla.firefox",
            "com.apple.Safari",
            "company.thebrowser.Browser",
        ],
        kind: AppKind::Browser,
        default_enabled: false,
    },
    CatalogEntry {
        id: "discord",
        display_name: "Discord",
        windows: &["discord.exe"],
        macos: &["com.hnc.Discord"],
        kind: AppKind::Native,
        default_enabled: false,
    },
    CatalogEntry {
        id: "facetime",
        display_name: "FaceTime",
        windows: &[],
        macos: &["com.apple.FaceTime"],
        kind: AppKind::Native,
        default_enabled: true,
    },
    CatalogEntry {
        id: "skype",
        display_name: "Skype",
        windows: &["skype.exe"],
        macos: &["com.skype.skype"],
        kind: AppKind::Native,
        default_enabled: false,
    },
];

/// Trigo itself uses the microphone for dictation: never a meeting.
const OWN_APP_IDS: &[&str] = &[
    "trigo-whispr.exe",
    "trigo whispr.exe",
    "com.trigo.trigo-whispr",
];

pub fn entry(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

pub fn is_own_app(user: &MicUser) -> bool {
    user.pid == Some(std::process::id())
        || OWN_APP_IDS
            .iter()
            .any(|own| own.eq_ignore_ascii_case(&user.app_id))
}

/// `app_id` equals `prefix`, or is one of its sub-identifiers.
fn bundle_matches(app_id: &str, prefix: &str) -> bool {
    let (app_id, prefix) = (app_id.to_ascii_lowercase(), prefix.to_ascii_lowercase());
    app_id == prefix
        || app_id
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// The catalog app a microphone user belongs to, if any. On macOS the
/// longest matching prefix wins.
pub fn match_app(user: &MicUser, platform: Platform) -> Option<&'static CatalogEntry> {
    if is_own_app(user) {
        return None;
    }
    let id = user.app_id.as_str();
    match platform {
        Platform::Windows => CATALOG
            .iter()
            .find(|e| e.windows.iter().any(|p| p.eq_ignore_ascii_case(id))),
        Platform::MacOs => CATALOG
            .iter()
            .filter_map(|e| {
                e.macos
                    .iter()
                    .filter(|p| bundle_matches(id, p))
                    .map(|p| p.len())
                    .max()
                    .map(|len| (e, len))
            })
            .max_by_key(|(_, len)| *len)
            .map(|(e, _)| e),
        Platform::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(app_id: &str, platform: Platform) -> Option<&'static str> {
        match_app(&MicUser::new(app_id), platform).map(|e| e.id)
    }

    #[test]
    fn windows_matches_executables_and_packages_ignoring_case() {
        assert_eq!(matched("ms-teams.exe", Platform::Windows), Some("teams"));
        assert_eq!(
            matched("msteams_8wekyb3d8bbwe", Platform::Windows),
            Some("teams")
        );
        assert_eq!(
            matched("Teams.exe", Platform::Windows),
            Some("teams_classic")
        );
        assert_eq!(matched("ZOOM.EXE", Platform::Windows), Some("zoom"));
        assert_eq!(matched("atmgr.exe", Platform::Windows), Some("webex"));
        assert_eq!(matched("chrome.exe", Platform::Windows), Some("browser"));
        assert_eq!(matched("notepad.exe", Platform::Windows), None);
        // A macOS bundle ID means nothing on Windows.
        assert_eq!(matched("us.zoom.xos", Platform::Windows), None);
    }

    #[test]
    fn macos_matches_bundle_prefixes_on_boundaries() {
        assert_eq!(
            matched("com.microsoft.teams2", Platform::MacOs),
            Some("teams")
        );
        assert_eq!(
            matched("com.microsoft.teams2.helper", Platform::MacOs),
            Some("teams")
        );
        assert_eq!(
            matched("com.microsoft.teams", Platform::MacOs),
            Some("teams_classic")
        );
        assert_eq!(
            matched("cisco-systems.spark", Platform::MacOs),
            Some("webex")
        );
        assert_eq!(
            matched("com.apple.FaceTime", Platform::MacOs),
            Some("facetime")
        );
        assert_eq!(matched("us.zoom.xosx", Platform::MacOs), None);
        assert_eq!(matched("com.example.recorder", Platform::MacOs), None);
    }

    #[test]
    fn trigo_is_never_a_meeting_app() {
        assert_eq!(matched("trigo-whispr.exe", Platform::Windows), None);
        assert_eq!(matched("com.trigo.trigo-whispr", Platform::MacOs), None);
        let mut own = MicUser::new("zoom.exe");
        own.pid = Some(std::process::id());
        assert!(match_app(&own, Platform::Windows).is_none());
    }

    #[test]
    fn defaults_and_overrides() {
        let overrides = HashMap::from([("zoom".to_string(), false)]);
        assert!(!entry("zoom").unwrap().enabled(&overrides));
        assert!(entry("teams").unwrap().enabled(&overrides));
        assert!(!entry("browser").unwrap().enabled(&HashMap::new()));
        assert!(!entry("facetime").unwrap().available_on(Platform::Windows));
        let mut ids: Vec<_> = CATALOG.iter().map(|e| e.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
    }
}
