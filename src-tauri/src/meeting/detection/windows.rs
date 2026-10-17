//! Windows backend of [`MicUsageProbe`]: reads the privacy registry Windows
//! keeps for the "microphone in use" taskbar indicator.
//!
//! ```text
//! HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\
//! ├── <PackageFamilyName>          packaged (MSIX/Store) apps, e.g. MSTeams_8wekyb3d8bbwe
//! │     LastUsedTimeStart / LastUsedTimeStop   (QWORD FILETIME)
//! └── NonPackaged\
//!       └── C:#Program Files#App#app.exe       exe path, "\" written as "#"
//! ```
//!
//! An app is using the microphone while `LastUsedTimeStart != 0` and
//! `LastUsedTimeStop == 0`. Microsoft does not document this, but forensic
//! tools and microphone indicators rely on it. A crashed app can leave
//! `Stop == 0` behind, so a non-packaged entry only counts while a process
//! with that executable name is running.
//!
//! Only HKCU is read: until the per-machine keys (HKLM) are shown to matter,
//! they would only add entries no process check covers.

use super::MicUser;
use std::path::PathBuf;

/// Registry path of the microphone consent store, under HKCU.
#[cfg_attr(not(windows), allow(dead_code))]
const MICROPHONE_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Sub-key listing the apps that are not packaged.
#[cfg_attr(not(windows), allow(dead_code))]
const NON_PACKAGED: &str = "NonPackaged";

/// `LastUsedTimeStart` / `LastUsedTimeStop` of an entry mean "in use now".
#[cfg_attr(not(windows), allow(dead_code))]
fn in_use(start: u64, stop: u64) -> bool {
    start != 0 && stop == 0
}

/// The executable path of a `NonPackaged` sub-key name
/// (`C:#Program Files#Zoom#bin#Zoom.exe` → `C:\Program Files\Zoom\bin\Zoom.exe`).
#[cfg_attr(not(windows), allow(dead_code))]
fn decode_non_packaged(key_name: &str) -> String {
    key_name.replace('#', "\\")
}

/// The `app_id` of a `NonPackaged` sub-key: the lowercase executable name.
#[cfg_attr(not(windows), allow(dead_code))]
fn non_packaged_app_id(key_name: &str) -> Option<String> {
    let exe = key_name.rsplit(['#', '\\', '/']).next()?.trim();
    (!exe.is_empty()).then(|| exe.to_lowercase())
}

/// A non-packaged entry in use, by executable.
#[cfg_attr(not(windows), allow(dead_code))]
fn non_packaged_user(key_name: &str) -> Option<MicUser> {
    let mut user = MicUser::new(non_packaged_app_id(key_name)?);
    user.exe_path = Some(PathBuf::from(decode_non_packaged(key_name)));
    Some(user)
}

#[cfg(windows)]
pub use probe::WindowsMicProbe;

#[cfg(windows)]
mod probe {
    use super::*;
    use crate::meeting::detection::MicUsageProbe;
    use anyhow::{Context, Result};
    use log::warn;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    /// The consent store could not be opened: logged once per run, as a
    /// probe is built again for every `supported()` check.
    static UNAVAILABLE_LOGGED: AtomicBool = AtomicBool::new(false);

    pub struct WindowsMicProbe {
        /// `None` when the consent store is missing or not readable.
        microphone: Option<RegKey>,
        /// Lowercase name of our own executable, never reported.
        own_exe: Option<String>,
    }

    impl WindowsMicProbe {
        pub fn new() -> Self {
            let microphone = match RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(MICROPHONE_KEY, KEY_READ)
            {
                Ok(key) => Some(key),
                Err(e) => {
                    if !UNAVAILABLE_LOGGED.swap(true, Ordering::Relaxed) {
                        warn!("Meeting detection unavailable: cannot read the microphone consent store: {e}");
                    }
                    None
                }
            };
            let own_exe = std::env::current_exe().ok().and_then(|p| {
                p.file_name()
                    .map(|name| name.to_string_lossy().to_lowercase())
            });
            Self {
                microphone,
                own_exe,
            }
        }

        fn is_own(&self, user: &MicUser) -> bool {
            self.own_exe.as_deref() == Some(user.app_id.as_str())
        }
    }

    impl Default for WindowsMicProbe {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Whether the entry's `LastUsedTimeStart` / `LastUsedTimeStop` say it is
    /// in use. A missing value counts as 0.
    fn entry_in_use(key: &RegKey) -> bool {
        let time = |name| key.get_value::<u64, _>(name).unwrap_or(0);
        in_use(time("LastUsedTimeStart"), time("LastUsedTimeStop"))
    }

    /// Sub-keys of `parent` currently in use, by name. Entries that vanish
    /// or cannot be read mid-enumeration are skipped.
    fn in_use_subkeys(parent: &RegKey, skip: Option<&str>) -> Vec<String> {
        parent
            .enum_keys()
            .filter_map(|name| name.ok())
            .filter(|name| skip.is_none_or(|skip| !name.eq_ignore_ascii_case(skip)))
            .filter(|name| {
                parent
                    .open_subkey_with_flags(name, KEY_READ)
                    .is_ok_and(|key| entry_in_use(&key))
            })
            .collect()
    }

    /// Lowercase executable name → a PID, for every running process.
    fn running_processes() -> Result<HashMap<String, u32>> {
        struct Snapshot(HANDLE);
        impl Drop for Snapshot {
            fn drop(&mut self) {
                // SAFETY: the handle came from CreateToolhelp32Snapshot and is
                // closed only here.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }

        // SAFETY: plain Win32 calls on a snapshot handle we own, with a
        // properly sized PROCESSENTRY32W.
        unsafe {
            let snapshot = Snapshot(
                CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                    .context("CreateToolhelp32Snapshot failed")?,
            );
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut processes = HashMap::new();
            let mut more = Process32FirstW(snapshot.0, &mut entry).is_ok();
            while more {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
                processes.entry(name).or_insert(entry.th32ProcessID);
                more = Process32NextW(snapshot.0, &mut entry).is_ok();
            }
            Ok(processes)
        }
    }

    impl MicUsageProbe for WindowsMicProbe {
        fn active_mic_users(&mut self) -> Result<Vec<MicUser>> {
            let Some(microphone) = &self.microphone else {
                return Ok(Vec::new());
            };

            let mut users: Vec<MicUser> = in_use_subkeys(microphone, Some(NON_PACKAGED))
                .into_iter()
                .map(MicUser::new)
                .collect();

            let non_packaged: Vec<MicUser> = microphone
                .open_subkey_with_flags(NON_PACKAGED, KEY_READ)
                .map(|key| in_use_subkeys(&key, None))
                .unwrap_or_default()
                .iter()
                .filter_map(|name| non_packaged_user(name))
                .filter(|user| !self.is_own(user))
                .collect();

            // Guard against a value left at "in use" by a crashed app; only
            // pays for a process snapshot when there is a candidate.
            if !non_packaged.is_empty() {
                match running_processes() {
                    Ok(processes) => {
                        for mut user in non_packaged {
                            match processes.get(&user.app_id) {
                                Some(&pid) => {
                                    user.pid = Some(pid);
                                    users.push(user);
                                }
                                None => {
                                    log::trace!(
                                        "Ignoring stale microphone use by {} (not running)",
                                        user.app_id
                                    )
                                }
                            }
                        }
                    }
                    // Without the check, trust the registry.
                    Err(e) => {
                        log::debug!("Process snapshot failed, skipping the stale check: {e}");
                        users.extend(non_packaged);
                    }
                }
            }

            // The same exe can be listed under several install paths.
            let mut seen = HashSet::new();
            users.retain(|user| seen.insert(user.app_id.clone()));
            Ok(users)
        }

        fn supported(&self) -> bool {
            self.microphone.is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_packaged_paths_are_decoded() {
        assert_eq!(
            decode_non_packaged(r"C:#Users#me#AppData#Roaming#Zoom#bin#Zoom.exe"),
            r"C:\Users\me\AppData\Roaming\Zoom\bin\Zoom.exe"
        );
        assert_eq!(
            decode_non_packaged(r"C:#Program Files#Slack#slack.exe"),
            r"C:\Program Files\Slack\slack.exe"
        );
    }

    #[test]
    fn non_packaged_app_id_is_the_lowercase_exe_name() {
        assert_eq!(
            non_packaged_app_id(r"C:#Users#me#AppData#Roaming#Zoom#bin#Zoom.exe").as_deref(),
            Some("zoom.exe")
        );
        assert_eq!(
            non_packaged_app_id(r"C:#Program Files (x86)#Microsoft#Edge#Application#msedge.exe")
                .as_deref(),
            Some("msedge.exe")
        );
        // Per-user and per-machine installs compare the same.
        assert_eq!(
            non_packaged_app_id(r"C:#Program Files#Zoom#bin#Zoom.exe"),
            non_packaged_app_id(r"C:#Users#me#AppData#Roaming#Zoom#bin#ZOOM.EXE")
        );
        assert_eq!(
            non_packaged_app_id("Teams.exe").as_deref(),
            Some("teams.exe")
        );
        assert_eq!(non_packaged_app_id(r"C:#Apps#"), None);
    }

    #[test]
    fn non_packaged_user_keeps_the_exe_path() {
        let user = non_packaged_user(r"C:#Program Files#Zoom#bin#Zoom.exe").unwrap();
        assert_eq!(user.app_id, "zoom.exe");
        assert_eq!(
            user.exe_path,
            Some(PathBuf::from(r"C:\Program Files\Zoom\bin\Zoom.exe"))
        );
        assert_eq!(user.pid, None);
    }

    #[test]
    fn in_use_needs_a_start_and_no_stop() {
        assert!(in_use(133_000_000_000_000_000, 0));
        assert!(!in_use(133_000_000_000_000_000, 133_000_000_100_000_000));
        // Never used.
        assert!(!in_use(0, 0));
        assert!(!in_use(0, 133_000_000_000_000_000));
    }
}
