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

type Key = (String, i32, u64);

struct Job {
    key: Key,
    wanted: Box<dyn Fn() -> bool>,
    done: Box<dyn Fn(gdk::Texture)>,
}

#[derive(Default)]
struct Inner {
    cache: RefCell<HashMap<Key, gdk::Texture>>,
    queue: RefCell<VecDeque<Job>>,
    active: Cell<usize>,
}

#[derive(Clone, Default)]
pub struct Thumbnailer(Rc<Inner>);

/// Decide which file to decode: an existing freedesktop thumbnail if present,
/// otherwise the file itself when it is an image of sane size.
pub fn source_for(info: &gio::FileInfo, full_path: &std::path::Path) -> Option<String> {
    // Cloud drives are fetched on demand; decoding every image would download them all.
    if full_path.starts_with(crate::integrations::cloud::base_dir()) {
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
        if let Some(t) = self.0.cache.borrow().get(&key) {
            done(t.clone());
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
                    let mut cache = this.0.cache.borrow_mut();
                    if cache.len() >= MAX_CACHE {
                        cache.clear();
                    }
                    cache.insert(job.key.clone(), tex.clone());
                    drop(cache);
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

fn decode(path: &str, size: i32) -> Option<Decoded> {
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
