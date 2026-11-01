//! Network locations through GVfs (SFTP, SMB, FTP, WebDAV, NFS, …).
//!
//! Remote folders are mounted by GVfs and browsed through its FUSE path
//! (`/run/user/<uid>/gvfs/...`), so every view and file operation works on
//! them unchanged. This needs `gvfs` (and its FUSE daemon) installed.

use gtk::gio::{self, prelude::*};
use std::path::PathBuf;

pub const PROTOCOLS: &[(&str, &str)] = &[
    ("sftp", "SFTP (SSH)"),
    ("google-drive", "Google Drive (GNOME online account)"),
    ("smb", "Windows share (SMB)"),
    ("ftp", "FTP"),
    ("davs", "WebDAV (secure)"),
    ("dav", "WebDAV"),
    ("nfs", "NFS"),
];

/// Validate user input and return a canonical URI.
pub fn normalize(input: &str) -> Result<String, String> {
    let s = input.trim();
    let Some((scheme, rest)) = s.split_once("://") else {
        return Err("Start with a protocol, e.g. sftp://host, smb://host/share or ftp://host".into());
    };
    let mut scheme = scheme.to_lowercase();
    if scheme == "ssh" {
        scheme = "sftp".into();
    }
    if !PROTOCOLS.iter().any(|(p, _)| *p == scheme) && !matches!(scheme.as_str(), "ftps" | "afp") {
        return Err(format!("“{scheme}” isn’t a supported protocol"));
    }
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.is_empty() {
        return Err("Enter a server name or address".into());
    }
    if s.contains(char::is_whitespace) {
        return Err("The address can’t contain spaces".into());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// A short human label such as "me@box (SFTP)".
pub fn label_for(uri: &str) -> String {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", uri));
    let authority = rest.split('/').next().unwrap_or(rest);
    let path = rest.trim_start_matches(authority).trim_matches('/');
    let proto = scheme.to_uppercase();
    if path.is_empty() { format!("{authority} ({proto})") } else { format!("{authority}/{path} ({proto})") }
}

/// (scheme, host) of a URI, for matching saved connections to live mounts.
pub fn scheme_host(uri: &str) -> Option<(String, String)> {
    let (scheme, rest) = uri.split_once("://")?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    Some((scheme.to_lowercase(), host.to_lowercase()))
}

/// Local path of an already-mounted location, if any.
pub async fn mounted_path(uri: &str) -> Option<PathBuf> {
    let f = gio::File::for_uri(uri);
    // A quick lookup in GVfs' mount table; it does not touch the network.
    f.find_enclosing_mount(gio::Cancellable::NONE).ok()?;
    f.path()
}

/// Mount `uri` (asking for credentials through GTK dialogs) and return the
/// folder to browse.
pub async fn connect(parent: &gtk::Window, uri: &str) -> Result<PathBuf, String> {
    if let Some(p) = mounted_path(uri).await {
        return Ok(p);
    }
    let f = gio::File::for_uri(uri);
    let op = gtk::MountOperation::new(Some(parent));
    if let Err(e) = f.mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&op)).await
        && !e.matches(gio::IOErrorEnum::AlreadyMounted)
    {
        return Err(e.message().to_string());
    }
    f.path().ok_or_else(|| "Connected, but GVfs has no folder for it. Install gvfs-fuse and sign in again.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_and_rejects() {
        assert_eq!(normalize("  SFTP://me@box/home ").unwrap(), "sftp://me@box/home");
        assert_eq!(normalize("ssh://box").unwrap(), "sftp://box");
        assert_eq!(normalize("smb://nas/share").unwrap(), "smb://nas/share");
        assert!(normalize("box").is_err());
        assert!(normalize("gopher://x").is_err());
        assert!(normalize("sftp://").is_err());
        assert!(normalize("sftp://me@/x").is_err());
        assert!(normalize("smb://nas/my share").is_err());
    }

    #[test]
    fn labels_and_matching() {
        assert_eq!(label_for("sftp://me@box"), "me@box (SFTP)");
        assert_eq!(label_for("smb://nas/media"), "nas/media (SMB)");
        assert_eq!(scheme_host("SFTP://me@Box:2222/x"), Some(("sftp".into(), "box".into())));
    }
}
