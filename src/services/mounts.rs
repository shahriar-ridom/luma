//! Mount / unmount / eject helpers (GIO volume monitor).

use gtk::gio::{self, prelude::*};
use std::path::PathBuf;

fn op(parent: &gtk::Window) -> gio::MountOperation {
    // GTK's mount operation supplies the password / question dialogs.
    gtk::MountOperation::new(Some(parent)).upcast()
}

pub async fn mount_volume(parent: &gtk::Window, volume: &gio::Volume) -> Result<PathBuf, String> {
    let o = op(parent);
    if let Err(e) = volume.mount_future(gio::MountMountFlags::NONE, Some(&o)).await
        && !e.matches(gio::IOErrorEnum::AlreadyMounted)
    {
        return Err(e.message().to_string());
    }
    volume
        .get_mount()
        .and_then(|m| m.root().path())
        .ok_or_else(|| "Mounted, but no folder is available for this volume.".to_string())
}

/// Eject when the device supports it (powers down removable media), else unmount.
pub async fn unmount(parent: &gtk::Window, mount: &gio::Mount) -> Result<(), String> {
    let o = op(parent);
    let r = if mount.can_eject() {
        mount.eject_with_operation_future(gio::MountUnmountFlags::NONE, Some(&o)).await
    } else {
        mount.unmount_with_operation_future(gio::MountUnmountFlags::NONE, Some(&o)).await
    };
    r.map_err(|e| e.message().to_string())
}
