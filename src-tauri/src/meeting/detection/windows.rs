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
//! A reading the probe cannot trust (an enumeration or access error, a value
//! of the wrong type, a used entry without a stop time) fails the whole poll
//! instead of dropping the entry: the service then skips that tick, so a
//! registry hiccup neither starts nor ends a meeting.
//!
//! Only HKCU is read: until the per-machine keys (HKLM) are shown to matter,
//! they would only add entries no process check covers.

// Off Windows, only the tests use the registry logic.
#![cfg_attr(not(windows), allow(dead_code))]

use super::MicUser;
use std::io;
use std::path::PathBuf;

/// Registry path of the microphone consent store, under HKCU.
const MICROPHONE_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Sub-key listing the apps that are not packaged.
const NON_PACKAGED: &str = "NonPackaged";

/// `LastUsedTimeStart` / `LastUsedTimeStop` of an entry mean "in use now".
fn in_use(start: u64, stop: u64) -> bool {
    start != 0 && stop == 0
}

/// The executable path of a `NonPackaged` sub-key name
/// (`C:#Program Files#Zoom#bin#Zoom.exe` → `C:\Program Files\Zoom\bin\Zoom.exe`).
fn decode_non_packaged(key_name: &str) -> String {
    key_name.replace('#', "\\")
}

/// The `app_id` of a `NonPackaged` sub-key: the lowercase executable name.
fn non_packaged_app_id(key_name: &str) -> Option<String> {
    let exe = key_name.rsplit(['#', '\\', '/']).next()?.trim();
    (!exe.is_empty()).then(|| exe.to_lowercase())
}

/// A non-packaged entry in use, by executable.
fn non_packaged_user(key_name: &str) -> Option<MicUser> {
    let mut user = MicUser::new(non_packaged_app_id(key_name)?);
    user.exe_path = Some(PathBuf::from(decode_non_packaged(key_name)));
    Some(user)
}

const LAST_USED_START: &str = "LastUsedTimeStart";
const LAST_USED_STOP: &str = "LastUsedTimeStop";

/// Win32 `ERROR_KEY_DELETED`: the key was deleted after it was opened.
const ERROR_KEY_DELETED: i32 = 1018;

/// The registry operations the probe needs, so that its handling of registry
/// errors is testable off Windows.
trait ConsentKey: Sized {
    /// Names of the sub-keys; fails on the first enumeration error.
    fn subkey_names(&self) -> io::Result<Vec<String>>;
    /// `ErrorKind::NotFound` when there is no such sub-key.
    fn open(&self, name: &str) -> io::Result<Self>;
    /// `ErrorKind::NotFound` when the value is missing.
    fn qword(&self, name: &str) -> io::Result<u64>;
}

/// Collects an enumeration up to its first error. winreg's iterator moves to
/// the next index after an error and only ends on "no more items", so
/// skipping errors would never end it while the error persists.
fn collect_names(names: impl Iterator<Item = io::Result<String>>) -> io::Result<Vec<String>> {
    names.collect()
}

fn with_entry(e: io::Error, entry: &str) -> io::Error {
    io::Error::new(e.kind(), format!("{entry}: {e}"))
}

/// Whether entry `name` of `parent` is in use; `None` when it vanished since
/// it was listed.
fn entry_in_use<K: ConsentKey>(parent: &K, name: &str) -> io::Result<Option<bool>> {
    let key = match parent.open(name) {
        Ok(key) => key,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(with_entry(e, name)),
    };
    let start = match key.qword(LAST_USED_START) {
        Ok(start) => start,
        // Never used the microphone.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Some(false)),
        Err(e) if e.raw_os_error() == Some(ERROR_KEY_DELETED) => return Ok(None),
        Err(e) => return Err(with_entry(e, name)),
    };
    if start == 0 {
        return Ok(Some(false));
    }
    // Used: only a valid stop time tells whether it still is.
    match key.qword(LAST_USED_STOP) {
        Ok(stop) => Ok(Some(in_use(start, stop))),
        Err(e) if e.raw_os_error() == Some(ERROR_KEY_DELETED) => Ok(None),
        Err(e) => Err(with_entry(e, &format!("{name}\\{LAST_USED_STOP}"))),
    }
}

/// Names of the sub-keys of `parent` in use, except `skip`.
fn in_use_entries<K: ConsentKey>(parent: &K, skip: Option<&str>) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for name in parent.subkey_names()? {
        if skip.is_some_and(|skip| name.eq_ignore_ascii_case(skip)) {
            continue;
        }
        if entry_in_use(parent, &name)? == Some(true) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Consent store entries in use, by sub-key name.
#[derive(Debug, Default, PartialEq, Eq)]
struct InUse {
    /// Package family names.
    packaged: Vec<String>,
    /// Encoded executable paths.
    non_packaged: Vec<String>,
}

/// Reads the microphone consent store. Any error means "unknown", never
/// "not in use".
fn scan<K: ConsentKey>(microphone: &K) -> io::Result<InUse> {
    let packaged = in_use_entries(microphone, Some(NON_PACKAGED))?;
    let non_packaged = match microphone.open(NON_PACKAGED) {
        Ok(key) => in_use_entries(&key, None)?,
        // No non-packaged app ever asked for the microphone.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(with_entry(e, NON_PACKAGED)),
    };
    Ok(InUse {
        packaged,
        non_packaged,
    })
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

    fn open_store() -> io::Result<RegKey> {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(MICROPHONE_KEY, KEY_READ)
    }

    pub struct WindowsMicProbe {
        /// Handle on the consent store; dropped after a failed read so the
        /// next poll reopens it (the key may have been deleted and recreated).
        microphone: Option<RegKey>,
        /// The consent store could be opened when the probe was built.
        supported: bool,
        /// Lowercase name of our own executable, never reported.
        own_exe: Option<String>,
    }

    impl WindowsMicProbe {
        pub fn new() -> Self {
            let microphone = match open_store() {
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
                supported: microphone.is_some(),
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

    impl ConsentKey for RegKey {
        fn subkey_names(&self) -> io::Result<Vec<String>> {
            collect_names(self.enum_keys())
        }

        fn open(&self, name: &str) -> io::Result<Self> {
            self.open_subkey_with_flags(name, KEY_READ)
        }

        fn qword(&self, name: &str) -> io::Result<u64> {
            self.get_value(name)
        }
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
            let microphone = match self.microphone.take() {
                Some(key) => key,
                None => open_store().context("cannot open the microphone consent store")?,
            };
            let in_use = scan(&microphone).context("cannot read the microphone consent store")?;
            self.microphone = Some(microphone);

            let mut users: Vec<MicUser> = in_use.packaged.into_iter().map(MicUser::new).collect();
            let non_packaged: Vec<MicUser> = in_use
                .non_packaged
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
            self.supported
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

    /// An in-memory registry key; anything not listed is missing.
    #[derive(Clone, Default)]
    struct FakeKey {
        children: Vec<(&'static str, Child)>,
        values: Vec<(&'static str, Value)>,
        /// Every enumeration step fails, the way a deleted key does.
        enum_fails: bool,
    }

    #[derive(Clone)]
    enum Child {
        Key(FakeKey),
        Denied,
        /// Listed, but gone by the time it is opened.
        Vanished,
    }

    /// A stored value, or the error reading it gives.
    type Value = Result<u64, fn() -> io::Error>;

    fn key_deleted() -> io::Error {
        io::Error::from_raw_os_error(ERROR_KEY_DELETED)
    }

    fn not_a_qword() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "not a QWORD")
    }

    fn denied() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "access denied")
    }

    impl ConsentKey for FakeKey {
        fn subkey_names(&self) -> io::Result<Vec<String>> {
            if self.enum_fails {
                // Never runs out: only stopping at the first error ends it.
                return collect_names(std::iter::repeat_with(|| Err(key_deleted())));
            }
            collect_names(self.children.iter().map(|(name, _)| Ok(name.to_string())))
        }

        fn open(&self, name: &str) -> io::Result<Self> {
            match self
                .children
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
            {
                Some((_, Child::Key(key))) => Ok(key.clone()),
                Some((_, Child::Denied)) => Err(denied()),
                Some((_, Child::Vanished)) | None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        fn qword(&self, name: &str) -> io::Result<u64> {
            match self.values.iter().find(|(n, _)| *n == name) {
                Some((_, Ok(v))) => Ok(*v),
                Some((_, Err(error))) => Err(error()),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
    }

    const USED: u64 = 133_000_000_000_000_000;
    const ENDED: u64 = 133_000_000_100_000_000;

    fn entry(start: u64, stop: u64) -> Child {
        Child::Key(FakeKey {
            values: vec![(LAST_USED_START, Ok(start)), (LAST_USED_STOP, Ok(stop))],
            ..Default::default()
        })
    }

    fn store(children: Vec<(&'static str, Child)>) -> FakeKey {
        FakeKey {
            children,
            ..Default::default()
        }
    }

    #[test]
    fn scan_lists_packaged_and_non_packaged_entries_in_use() {
        let microphone = store(vec![
            ("MSTeams_8wekyb3d8bbwe", entry(USED, 0)),
            (
                "Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe",
                entry(USED, ENDED),
            ),
            (
                NON_PACKAGED,
                Child::Key(store(vec![
                    (r"C:#Program Files#Zoom#bin#Zoom.exe", entry(USED, 0)),
                    (r"C:#Apps#old.exe", entry(USED, ENDED)),
                    (r"C:#Apps#never.exe", entry(0, 0)),
                ])),
            ),
        ]);
        assert_eq!(
            scan(&microphone).unwrap(),
            InUse {
                packaged: vec!["MSTeams_8wekyb3d8bbwe".into()],
                non_packaged: vec![r"C:#Program Files#Zoom#bin#Zoom.exe".into()],
            }
        );
        // No NonPackaged key at all: nothing non-packaged, not an error.
        let packaged_only = store(vec![("MSTeams_8wekyb3d8bbwe", entry(USED, 0))]);
        assert!(scan(&packaged_only).unwrap().non_packaged.is_empty());
    }

    #[test]
    fn a_persistent_enumeration_error_fails_the_scan_at_once() {
        let microphone = FakeKey {
            enum_fails: true,
            ..Default::default()
        };
        assert!(scan(&microphone).is_err());
        let non_packaged = store(vec![(
            NON_PACKAGED,
            Child::Key(FakeKey {
                enum_fails: true,
                ..Default::default()
            }),
        )]);
        assert!(scan(&non_packaged).is_err());
    }

    #[test]
    fn an_unreadable_entry_fails_the_scan_instead_of_dropping_the_app() {
        // Otherwise a running call would look over and get stopped.
        let denied_entry = store(vec![
            ("MSTeams_8wekyb3d8bbwe", entry(USED, 0)),
            (r"zoom", Child::Denied),
        ]);
        assert!(scan(&denied_entry).is_err());
        let denied_non_packaged = store(vec![(NON_PACKAGED, Child::Denied)]);
        assert!(scan(&denied_non_packaged).is_err());
        let denied_value = store(vec![(
            "MSTeams_8wekyb3d8bbwe",
            Child::Key(FakeKey {
                values: vec![(LAST_USED_START, Ok(USED)), (LAST_USED_STOP, Err(denied))],
                ..Default::default()
            }),
        )]);
        assert!(scan(&denied_value).is_err());
        let wrong_type = store(vec![(
            "MSTeams_8wekyb3d8bbwe",
            Child::Key(FakeKey {
                values: vec![(LAST_USED_START, Err(not_a_qword))],
                ..Default::default()
            }),
        )]);
        assert!(scan(&wrong_type).is_err());
    }

    #[test]
    fn a_used_entry_without_a_stop_time_is_not_taken_as_in_use() {
        // Otherwise an old start time would prompt for a meeting.
        let microphone = store(vec![(
            "MSTeams_8wekyb3d8bbwe",
            Child::Key(FakeKey {
                values: vec![(LAST_USED_START, Ok(USED))],
                ..Default::default()
            }),
        )]);
        assert!(scan(&microphone).is_err());
    }

    #[test]
    fn entries_never_used_or_gone_are_skipped() {
        let microphone = store(vec![
            // Never asked for the microphone: no times at all.
            ("Never", Child::Key(FakeKey::default())),
            // Never used, so its stop time does not matter.
            (
                "Unused",
                Child::Key(FakeKey {
                    values: vec![(LAST_USED_START, Ok(0)), (LAST_USED_STOP, Err(denied))],
                    ..Default::default()
                }),
            ),
            // Removed between the listing and the read.
            ("Uninstalled", Child::Vanished),
            (
                "DeletedWhileRead",
                Child::Key(FakeKey {
                    values: vec![
                        (LAST_USED_START, Ok(USED)),
                        (LAST_USED_STOP, Err(key_deleted)),
                    ],
                    ..Default::default()
                }),
            ),
            ("MSTeams_8wekyb3d8bbwe", entry(USED, 0)),
        ]);
        assert_eq!(
            scan(&microphone).unwrap().packaged,
            vec!["MSTeams_8wekyb3d8bbwe".to_string()]
        );
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
