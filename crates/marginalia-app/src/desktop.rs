//! Linux desktop integration.
//!
//! An AppImage is a single file that installs nothing, so the desktop has
//! no launcher or icon for it: the taskbar shows a generic icon, it is not
//! in the application menu and books cannot be opened with it. When run as
//! an AppImage, Marginalia adds them for the current user, pointing at the
//! AppImage file. Settings shows whether Marginalia is in the menu, and
//! adds or removes it.

use std::fs;
use std::path::{Path, PathBuf};

/// The name GTK gives the window (the program name), which desktops match
/// to `<name>.desktop` and its `StartupWMClass` to find its icon.
const APP_ID: &str = "marginalia-app";

const MIME_TYPES: &str = "application/epub+zip;application/x-mobipocket-ebook;\
    application/vnd.amazon.mobi8-ebook;application/x-fictionbook+xml;\
    application/x-zip-compressed-fb2;";

const ICONS: &[(&str, &[u8])] = &[
    ("32x32", include_bytes!("../icons/32x32.png")),
    ("128x128", include_bytes!("../icons/128x128.png")),
    ("256x256", include_bytes!("../icons/128x128@2x.png")),
];

fn desktop_entry(appimage: &Path) -> String {
    let exec = appimage.to_string_lossy().replace('"', "\\\"");
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Marginalia\n\
         Comment=EPUB/MOBI/PRC reader with text-to-speech\n\
         Exec=\"{exec}\" %F\n\
         Icon={APP_ID}\n\
         StartupWMClass={APP_ID}\n\
         Categories=Office;\n\
         Terminal=false\n\
         MimeType={MIME_TYPES}\n"
    )
}

/// Write `contents` to `path` unless it already holds them.
fn write_if_changed(path: &Path, contents: &[u8]) -> std::io::Result<bool> {
    if fs::read(path).is_ok_and(|old| old == contents) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, contents)?;
    Ok(true)
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from)
}

/// Installed from the .deb (or another package): the system has the
/// launcher already.
fn packaged() -> bool {
    Path::new("/usr/share/applications/Marginalia.desktop").exists()
}

fn launcher_path() -> Option<PathBuf> {
    Some(data_home()?.join(format!("applications/{APP_ID}.desktop")))
}

fn refresh_menu() {
    // Let the desktop pick up the new entry and file types now (KDE caches
    // launchers); each tool is harmless if missing. Not in tests, where the
    // tools' exit interrupts other tests' network calls.
    if cfg!(test) {
        return;
    }
    if let Some(dir) = launcher_path().as_deref().and_then(Path::parent) {
        let _ = std::process::Command::new("update-desktop-database")
            .arg(dir)
            .status();
    }
    for tool in ["kbuildsycoca6", "kbuildsycoca5"] {
        if std::process::Command::new(tool)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            break;
        }
    }
}

fn write_launcher(appimage: &Path) -> std::io::Result<bool> {
    let data = data_home().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let mut changed = false;
    for (size, png) in ICONS {
        let path = data.join(format!("icons/hicolor/{size}/apps/{APP_ID}.png"));
        changed |= write_if_changed(&path, png)?;
    }
    let path = launcher_path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    changed |= write_if_changed(&path, desktop_entry(appimage).as_bytes())?;
    Ok(changed)
}

/// Give the program the name its launcher is matched by. GTK takes it from
/// the command line, which in an AppImage is its wrapper (`AppRun`,
/// `AppRun.wrapped`); the window then carries that name as its Wayland app
/// id and X11 class, and KDE shows a generic icon in the taskbar. Must run
/// before GTK starts.
pub fn name_program() {
    glib::set_prgname(Some(APP_ID));
}

/// Add or refresh the launcher and icon when running as an AppImage.
pub fn integrate() {
    let Some(appimage) = appimage() else { return };
    if packaged() {
        return;
    }
    match write_launcher(&appimage) {
        Ok(true) => refresh_menu(),
        Ok(false) => {}
        Err(e) => eprintln!("could not add the Marginalia launcher: {e}"),
    }
}

#[derive(serde::Serialize)]
pub struct Status {
    /// Running as an AppImage, whose launcher Marginalia manages.
    appimage: bool,
    /// In the application menu.
    launcher: bool,
}

pub fn status() -> Status {
    Status {
        appimage: appimage().is_some() && !packaged(),
        launcher: packaged() || launcher_path().is_some_and(|p| p.exists()),
    }
}

/// Add Marginalia to the application menu, or take it out.
pub fn set_launcher(on: bool) -> std::io::Result<()> {
    let Some(appimage) = appimage().filter(|_| !packaged()) else {
        return Err(std::io::Error::other(
            "Marginalia was installed as a package, which manages its menu entry",
        ));
    };
    if on {
        write_launcher(&appimage)?;
    } else if let Some(path) = launcher_path().filter(|p| p.exists()) {
        fs::remove_file(path)?;
    }
    refresh_menu();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_runs_the_appimage() {
        let entry = desktop_entry(Path::new("/home/me/Apps/Marginalia.AppImage"));
        assert!(entry.contains("Exec=\"/home/me/Apps/Marginalia.AppImage\" %F\n"));
        assert!(entry.contains("Icon=marginalia-app\n"));
        assert!(entry.contains(
            "MimeType=application/epub+zip;application/x-mobipocket-ebook;application/vnd"
        ));
    }

    #[test]
    fn adds_and_removes_the_menu_entry() {
        let dir = std::env::temp_dir().join(format!("marginalia-desktop-{}", std::process::id()));
        std::env::set_var("XDG_DATA_HOME", &dir);
        std::env::set_var("APPIMAGE", "/opt/Marginalia.AppImage");
        if packaged() {
            return; // a real install on this machine owns the entry
        }
        set_launcher(true).unwrap();
        let entry = dir.join("applications/marginalia-app.desktop");
        assert!(fs::read_to_string(&entry)
            .unwrap()
            .contains("/opt/Marginalia.AppImage"));
        assert!(dir
            .join("icons/hicolor/256x256/apps/marginalia-app.png")
            .exists());
        let s = status();
        assert!(s.appimage && s.launcher);
        set_launcher(false).unwrap();
        assert!(!entry.exists());
        assert!(!status().launcher);
        let _ = fs::remove_dir_all(dir);
    }
}
