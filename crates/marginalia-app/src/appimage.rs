//! An AppImage is a single file that installs nothing, so the desktop has
//! no launcher or icon for it: the taskbar shows a generic icon and books
//! cannot be opened with it. When run as an AppImage, Marginalia adds them
//! for the current user, pointing at the AppImage file.

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

/// Add or refresh the launcher and icon when running as an AppImage.
pub fn integrate() {
    let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
        return;
    };
    // The .deb is installed too: its launcher already names this window.
    if Path::new("/usr/share/applications/Marginalia.desktop").exists() {
        return;
    }
    let Some(data) = data_home() else { return };
    let apps = data.join("applications");
    let result = (|| -> std::io::Result<bool> {
        let mut changed = false;
        for (size, png) in ICONS {
            let path = data.join(format!("icons/hicolor/{size}/apps/{APP_ID}.png"));
            changed |= write_if_changed(&path, png)?;
        }
        let entry = desktop_entry(&appimage);
        changed |= write_if_changed(&apps.join(format!("{APP_ID}.desktop")), entry.as_bytes())?;
        Ok(changed)
    })();
    match result {
        // Let the desktop pick up the new file types; harmless if missing.
        Ok(true) => {
            let _ = std::process::Command::new("update-desktop-database")
                .arg(&apps)
                .status();
        }
        Ok(false) => {}
        Err(e) => eprintln!("could not add the Marginalia launcher: {e}"),
    }
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
}
