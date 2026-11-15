//! Keep local and remote raster images reusable across preview revisions.

mod remote;

use gpui::{App, Global, RenderImage, Window};
use image::{ImageFormat, ImageReader, Limits};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use url::Url;

/// Maximum encoded bytes read from one image.
const ENCODED_LIMIT: u64 = 8 * 1024 * 1024;
/// Maximum decoded cache size across editor windows.
const CACHE_BYTES: usize = 40 * 1024 * 1024;
/// Maximum bytes in a single decoded RGBA frame.
const FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Maximum cache entries and distinct URL lookups per shaped document.
const IMAGE_LIMIT: usize = 64;

/// What the current preview can display for an image.
#[derive(Clone)]
pub(super) enum ImageState {
    /// Decoded first frame available to paint.
    Ready(Arc<RenderImage>),
    /// A remote request is queued or still in progress.
    Pending,
    /// The image source could not be fetched, read or decoded.
    Failed,
}

/// Access to bounded raster loading for one layout pass.
pub(super) struct Images<'a> {
    /// Relative image URLs are resolved against the document's parent directory.
    document: Option<&'a Path>,
    /// Cached canonical directory and file URL for the first eligible image.
    base: Option<(PathBuf, Url)>,
    /// Image lookups already attempted during this layout.
    lookups: HashMap<String, ImageState>,
    /// Application image cache and other native windows.
    cx: &'a mut App,
    /// The active window's atlas, separate from the application's window registry during drawing.
    window: &'a mut Window,
}

impl<'a> Images<'a> {
    /// Resolve relative URLs against `document`, sharing cached rasters through `cx` and `window`.
    pub(super) fn new(document: Option<&'a Path>, window: &'a mut Window, cx: &'a mut App) -> Self {
        if !cx.has_global::<ImageCache>() {
            cx.set_global(ImageCache::default());
        }
        Self {
            document,
            base: None,
            lookups: HashMap::new(),
            cx,
            window,
        }
    }

    /// Return the raster at `url` or the pending/failed state for the layout.
    pub(super) fn get(&mut self, url: &str) -> ImageState {
        if let Some(image) = self.lookups.get(url) {
            return image.clone();
        }
        if self.lookups.len() >= IMAGE_LIMIT {
            return ImageState::Failed;
        }
        let image = if let Some(url) = remote::request_url(url) {
            remote::request(url, self.cx)
        } else {
            self.load_local(url)
                .map_or(ImageState::Failed, ImageState::Ready)
        };
        self.lookups.insert(url.into(), image.clone());
        image
    }

    /// Resolve and cache a local `url`, retiring unused rasters if necessary.
    fn load_local(&mut self, url: &str) -> Option<Arc<RenderImage>> {
        if url.starts_with(['/', '\\', '#', '?']) || Url::parse(url).is_ok() {
            return None;
        }
        if self.base.is_none() {
            let directory = self.document?.parent()?.canonicalize().ok()?;
            let url = Url::from_directory_path(&directory).ok()?;
            self.base = Some((directory, url));
        }
        let (directory, base) = self.base.as_ref()?;
        let path = relative_file(directory, base, url)?;
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > ENCODED_LIMIT {
            return None;
        }
        let key = ImageKey::Local(LocalImageKey {
            path: path.clone(),
            len: metadata.len(),
            modified: metadata.modified().ok()?,
        });
        let cache = &self.cx.global::<ImageCache>().0;
        if let Some(image) = cache.borrow_mut().get(&key) {
            return Some(image);
        }
        let (image, decoded_bytes) = decode_local(&path)?;
        let image = Arc::new(image);
        let (inserted, retired) = cache.borrow_mut().insert(key, image.clone(), decoded_bytes);
        for retired_image in retired {
            self.cx.drop_image(retired_image, Some(self.window));
        }
        inserted.then_some(image)
    }
}

/// Return the layout revision for completed remote requests in `cx`.
pub(in crate::grid::windows::editor::markdown) fn image_revision(cx: &App) -> u64 {
    cx.try_global::<ImageCache>()
        .map_or(0, |cache| cache.0.borrow().remote.revision)
}

/// Resolve the relative `url` against its file `base`, including URL escaping and symlinks.
fn relative_file(directory: &Path, base: &Url, url: &str) -> Option<PathBuf> {
    let path = base
        .join(url)
        .ok()?
        .to_file_path()
        .ok()?
        .canonicalize()
        .ok()?;
    path.starts_with(directory).then_some(path)
}

/// How the cache identifies a decoded image.
#[derive(PartialEq, Eq)]
enum ImageKey {
    /// A file and the metadata identifying its current contents.
    Local(LocalImageKey),
    /// Normalized HTTP(S) URL, without the fragment.
    Remote(Url),
}

/// Stamped file identity for reuse and refresh across document layout changes.
#[derive(PartialEq, Eq)]
struct LocalImageKey {
    /// Canonical file path under the document directory.
    path: PathBuf,
    /// Encoded size used with the modification time to identify changed files.
    len: u64,
    /// Last modification time.
    modified: SystemTime,
}

/// The cached raster and the bytes counted against the shared limit.
struct CachedImage {
    /// File identity when the raster was decoded.
    key: ImageKey,
    /// The first decoded frame, shared with any live preview layouts.
    image: Arc<RenderImage>,
    /// Decoded pixel bytes.
    bytes: usize,
}

/// Application-wide image resources, with native atlas eviction on cache replacement.
#[derive(Default)]
struct ImageCache(RefCell<Cache>);

impl Global for ImageCache {}

/// Decoded images from least recently to most recently used.
#[derive(Default)]
struct Cache {
    /// Only images unused by retained layouts are eviction candidates.
    entries: VecDeque<CachedImage>,
    /// Decoded bytes across the cache entries.
    bytes: usize,
    /// Outstanding remote requests and previously failed URLs.
    remote: remote::Requests,
}

impl Cache {
    /// Get the raster for `key` and move it to the most-recently used end.
    fn get(&mut self, key: &ImageKey) -> Option<Arc<RenderImage>> {
        let index = self.entries.iter().position(|entry| &entry.key == key)?;
        let entry = self.entries.remove(index)?;
        let image = Arc::clone(&entry.image);
        self.entries.push_back(entry);
        Some(image)
    }

    /// Keep the decoded `image` for `key` if `bytes` fit, returning released atlas images.
    fn insert(
        &mut self,
        key: ImageKey,
        image: Arc<RenderImage>,
        bytes: usize,
    ) -> (bool, Vec<Arc<RenderImage>>) {
        let mut retired = Vec::new();
        while self.entries.len() >= IMAGE_LIMIT || self.bytes + bytes > CACHE_BYTES {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| Arc::strong_count(&entry.image) == 1)
            else {
                return (false, retired);
            };
            let entry = self
                .entries
                .remove(index)
                .expect("index came from the entry queue");
            self.bytes -= entry.bytes;
            retired.push(entry.image);
        }
        self.bytes += bytes;
        self.entries.push_back(CachedImage { key, image, bytes });
        (true, retired)
    }
}

/// Read a bounded file at `path` and decode its first frame.
fn decode_local(path: &Path) -> Option<(RenderImage, usize)> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(ENCODED_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    decode(&bytes)
}

/// Decode the first supported frame in `bytes` and return it with its BGRA allocation size.
fn decode(bytes: &[u8]) -> Option<(RenderImage, usize)> {
    if bytes.len() as u64 > ENCODED_LIMIT {
        return None;
    }
    let format = image::guess_format(bytes).ok()?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(FRAME_BYTES as u64);
    let reader = || {
        let mut reader = ImageReader::with_format(BufReader::new(Cursor::new(bytes)), format);
        reader.limits(limits.clone());
        reader
    };
    let (width, height) = reader().into_dimensions().ok()?;
    if u64::from(width) * u64::from(height) > (FRAME_BYTES / 4) as u64 {
        return None;
    }
    let mut rgba = reader().decode().ok()?.into_rgba8();
    let bytes = rgba.as_raw().len();
    // GPUI uploads BGRA bytes to its sprite atlas.
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some((RenderImage::new([image::Frame::new(rgba)]), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_raster_urls_and_decodes_native_colors() {
        let temporary = tempfile::tempdir().unwrap();
        let documents = temporary.path().join("docs");
        fs::create_dir(&documents).unwrap();
        let file = documents.join("diagram red.png");
        image::RgbaImage::from_pixel(2, 1, image::Rgba([201, 33, 19, 255]))
            .save(&file)
            .unwrap();
        fs::write(temporary.path().join("outside.png"), b"").unwrap();

        let directory = documents.canonicalize().unwrap();
        let base = Url::from_directory_path(&directory).unwrap();
        let local = relative_file(&directory, &base, "diagram%20red.png?size=2#top").unwrap();
        assert_eq!(local, file.canonicalize().unwrap());
        assert!(relative_file(&directory, &base, "../outside.png").is_none());
        assert!(relative_file(&directory, &base, "%2e%2e/outside.png").is_none());
        let (image, decoded) = decode_local(&local).unwrap();
        assert_eq!(decoded, 8);
        assert_eq!(
            image.as_bytes(0).unwrap(),
            [19, 33, 201, 255, 19, 33, 201, 255]
        );
    }
}
