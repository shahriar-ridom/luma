//! Background thumbnail loader: bounded concurrency, in-memory cache,
//! lazy (jobs are dropped if the row scrolled away before they start).

use gtk::{gdk, gdk_pixbuf, gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
};

const MAX_ACTIVE: usize = 4;
const MAX_CACHE: usize = 600;
const MAX_SOURCE_BYTES: i64 = 64 * 1024 * 1024;
/// A small file can still declare a huge picture (a "decompression bomb").
const MAX_PIXELS: u64 = 100_000_000;

type Key = (String, i32, u64);

struct Job {
    key: Key,
    wanted: Box<dyn Fn() -> bool>,
    done: Box<dyn Fn(gdk::Texture)>,
}

/// Least-recently-used cache: when full, the entry that has gone unused the
/// longest is dropped, never the whole cache.
struct Lru<V> {
    map: HashMap<Key, V>,
    /// Keys from least to most recently used.
    order: VecDeque<Key>,
    cap: usize,
}

impl<V: Clone> Lru<V> {
    fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), order: VecDeque::new(), cap }
    }

    fn touch(&mut self, key: &Key) {
        if let Some(i) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(i).expect("position is in range");
            self.order.push_back(k);
        }
    }

    fn get(&mut self, key: &Key) -> Option<V> {
        let v = self.map.get(key).cloned()?;
        self.touch(key);
        Some(v)
    }

    fn insert(&mut self, key: Key, value: V) {
        if self.map.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        while self.map.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

struct Inner {
    cache: RefCell<Lru<gdk::Texture>>,
    queue: RefCell<VecDeque<Job>>,
    active: Cell<usize>,
}

impl Default for Inner {
    fn default() -> Self {
        Self { cache: RefCell::new(Lru::new(MAX_CACHE)), queue: RefCell::default(), active: Cell::new(0) }
    }
}

#[derive(Clone, Default)]
pub struct Thumbnailer(Rc<Inner>);

/// Mount points of network file systems (NFS, SMB, SSHFS, …) in `mountinfo`.
fn network_mount_roots(mountinfo: &str) -> Vec<std::path::PathBuf> {
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "9p",
        "ceph",
        "glusterfs",
        "afs",
        "davfs",
        "fuse.sshfs",
        "fuse.rclone",
    ];
    crate::services::storage::parse_mountinfo(mountinfo)
        .into_iter()
        .filter(|m| NETWORK.contains(&m.fstype.as_str()))
        .map(|m| std::path::PathBuf::from(m.target))
        .collect()
}

/// Is `path` on a remote file system (GVfs, a cloud drive, NFS, SMB, SSHFS)?
/// Reading every image there just to draw a thumbnail would download it all.
fn on_remote_fs(path: &std::path::Path) -> bool {
    use std::time::{Duration, Instant};
    thread_local! {
        static ROOTS: RefCell<Option<(Instant, Vec<std::path::PathBuf>)>> = const { RefCell::new(None) };
    }
    if crate::integrations::network::remote_trees().iter().any(|t| path.starts_with(t)) {
        return true;
    }
    ROOTS.with(|r| {
        let mut r = r.borrow_mut();
        if r.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(30)) {
            let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
            *r = Some((Instant::now(), network_mount_roots(&text)));
        }
        r.as_ref().is_some_and(|(_, roots)| roots.iter().any(|m| path.starts_with(m)))
    })
}

/// Decide which file to decode: an existing freedesktop thumbnail if present,
/// otherwise the file itself when it is an image of sane size.
pub fn source_for(info: &gio::FileInfo, full_path: &std::path::Path) -> Option<String> {
    if on_remote_fs(full_path) {
        return None;
    }
    if let Some(t) = info.attribute_byte_string("thumbnail::path") {
        return Some(t.to_string());
    }
    let ct = info.content_type()?;
    if ct.starts_with("image/") && info.size() < MAX_SOURCE_BYTES {
        return Some(full_path.to_string_lossy().into_owned());
    }
    None
}

impl Thumbnailer {
    /// Request a `size`×`size` thumbnail of `path`. `wanted` is polled right
    /// before decoding starts and again before delivery.
    pub fn request(
        &self,
        path: String,
        mtime: u64,
        size: i32,
        wanted: impl Fn() -> bool + 'static,
        done: impl Fn(gdk::Texture) + 'static,
    ) {
        let key = (path, size, mtime);
        let hit = self.0.cache.borrow_mut().get(&key);
        if let Some(t) = hit {
            done(t);
            return;
        }
        self.0.queue.borrow_mut().push_back(Job { key, wanted: Box::new(wanted), done: Box::new(done) });
        self.pump();
    }

    fn pump(&self) {
        while self.0.active.get() < MAX_ACTIVE {
            let Some(job) = self.0.queue.borrow_mut().pop_back() else { return };
            if !(job.wanted)() {
                continue;
            }
            self.0.active.set(self.0.active.get() + 1);
            let this = self.clone();
            let (path, size, _) = job.key.clone();
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || decode(&path, size)).await;
                this.0.active.set(this.0.active.get() - 1);
                if let Ok(Some(d)) = res {
                    let tex = to_texture(d);
                    this.0.cache.borrow_mut().insert(job.key.clone(), tex.clone());
                    if (job.wanted)() {
                        (job.done)(tex);
                    }
                }
                this.pump();
            });
        }
    }
}

use gtk::prelude::*;

type Decoded = (Vec<u8>, i32, i32, usize, bool);

/// Does the picture's header declare more pixels than we are willing to decode?
fn too_many_pixels(path: &str) -> bool {
    gdk_pixbuf::Pixbuf::file_info(path).is_some_and(|(_, w, h)| w.max(0) as u64 * h.max(0) as u64 > MAX_PIXELS)
}

fn decode(path: &str, size: i32) -> Option<Decoded> {
    if too_many_pixels(path) {
        return None;
    }
    let pb = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
    Some((pb.read_pixel_bytes().to_vec(), pb.width(), pb.height(), pb.rowstride() as usize, pb.has_alpha()))
}

fn to_texture((bytes, w, h, stride, alpha): Decoded) -> gdk::Texture {
    let fmt = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    gdk::MemoryTexture::new(w, h, fmt, &glib::Bytes::from_owned(bytes), stride).upcast()
}

/// Decode `path` off the main thread, scaled to fit `size`. Also returns the
/// original pixel dimensions. Used by the preview panel and gallery.
pub async fn load_texture(path: String, size: i32) -> Option<(gdk::Texture, (i32, i32))> {
    let r = gio::spawn_blocking(move || {
        let dims = gdk_pixbuf::Pixbuf::file_info(&path).map(|(_, w, h)| (w, h)).unwrap_or((0, 0));
        decode(&path, size).map(|d| (d, dims))
    })
    .await
    .ok()??;
    Some((to_texture(r.0), r.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u64) -> Key {
        (format!("/p/{n}"), 64, n)
    }

    #[test]
    fn lru_drops_the_least_recently_used_entry_only() {
        let mut c = Lru::new(3);
        for n in 1..=3 {
            c.insert(key(n), n);
        }
        assert_eq!(c.get(&key(1)), Some(1)); // 1 is now the freshest, 2 the oldest
        c.insert(key(4), 4);
        assert_eq!(c.get(&key(2)), None, "the oldest went");
        assert_eq!((c.get(&key(1)), c.get(&key(3)), c.get(&key(4))), (Some(1), Some(3), Some(4)));
        c.insert(key(4), 40); // replacing does not grow or evict
        assert_eq!(c.map.len(), 3);
        assert_eq!(c.get(&key(4)), Some(40));
    }

    #[test]
    fn huge_declared_pictures_are_refused_before_decoding() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/thumb");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A PNG declaring 50000 x 50000 pixels (2.5 gigapixels): just the header chunks, no picture data.
        let hex = "89504e470d0a1a0a0000000d494844520000c3500000c3500802000000c4cdaa9d0000000449444154789c6300fe711684";
        let bomb: Vec<u8> =
            (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
        let (bomb_path, small_path) = (dir.join("bomb.png"), dir.join("small.png"));
        std::fs::write(&bomb_path, bomb).unwrap();
        gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 8)
            .unwrap()
            .savev(&small_path, "png", &[])
            .unwrap();
        assert!(too_many_pixels(bomb_path.to_str().unwrap()));
        assert!(!too_many_pixels(small_path.to_str().unwrap()));
        assert!(decode(bomb_path.to_str().unwrap(), 128).is_none());
        assert!(decode(small_path.to_str().unwrap(), 128).is_some());
    }

    #[test]
    fn network_mounts_are_found_in_mountinfo() {
        let info = "\
36 25 0:30 / / rw - ext4 /dev/sda1 rw\n\
40 36 0:41 / /mnt/nas rw - nfs4 nas:/export rw\n\
41 36 0:42 / /mnt/box rw - fuse.sshfs me@box:/ rw\n\
42 36 0:43 / /mnt/usb rw - vfat /dev/sdb1 rw\n";
        let roots = network_mount_roots(info);
        assert_eq!(roots, [std::path::PathBuf::from("/mnt/nas"), std::path::PathBuf::from("/mnt/box")]);
    }
}
