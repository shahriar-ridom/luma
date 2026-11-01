//! Clipboard interop: GNOME/Nautilus `x-special/gnome-copied-files`, KDE cut
//! flag, GDK file lists and plain-text paths.

use gtk::{gdk, gio, glib, prelude::*};

const GNOME: &str = "x-special/gnome-copied-files";
const KDE_CUT: &str = "application/x-kde-cutselection";

pub fn set_files(cb: &gdk::Clipboard, files: &[gio::File], cut: bool) {
    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
    let text = files
        .iter()
        .map(|f| f.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| f.uri().to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut providers = vec![
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(files).to_value()),
        gdk::ContentProvider::for_bytes(GNOME, &glib::Bytes::from_owned(gnome.into_bytes())),
        gdk::ContentProvider::for_value(&text.to_value()),
    ];
    if cut {
        providers.push(gdk::ContentProvider::for_bytes(KDE_CUT, &glib::Bytes::from_static(b"1")));
    }
    let _ = cb.set_content(Some(&gdk::ContentProvider::new_union(&providers)));
}

pub fn clear(cb: &gdk::Clipboard) {
    let _ = cb.set_content(None::<&gdk::ContentProvider>);
}

async fn read_all(stream: &gio::InputStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(b) = stream.read_bytes_future(64 * 1024, glib::Priority::DEFAULT).await {
        if b.is_empty() {
            break;
        }
        out.extend_from_slice(&b);
        if out.len() > 16 << 20 {
            break;
        }
    }
    out
}

/// Returns the files on the clipboard and whether they were cut.
pub async fn get_files(cb: &gdk::Clipboard) -> Option<(Vec<gio::File>, bool)> {
    let formats = cb.formats();
    if formats.contain_mime_type(GNOME) {
        let (stream, _) = cb.read_future(&[GNOME], glib::Priority::DEFAULT).await.ok()?;
        let data = String::from_utf8(read_all(&stream).await).ok()?;
        let mut lines = data.lines();
        let cut = lines.next()? == "cut";
        let files: Vec<_> = lines.filter(|l| !l.is_empty()).map(gio::File::for_uri).collect();
        if !files.is_empty() {
            return Some((files, cut));
        }
    }
    if formats.contains_type(gdk::FileList::static_type()) {
        let v = cb.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await.ok()?;
        let files = v.get::<gdk::FileList>().ok()?.files();
        if !files.is_empty() {
            let cut = formats.contain_mime_type(KDE_CUT);
            return Some((files, cut));
        }
    }
    None
}
