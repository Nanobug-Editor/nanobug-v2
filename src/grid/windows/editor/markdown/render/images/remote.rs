//! Queue HTTP(S) rasters on the background executor and publish settled results to the preview.

use super::{ENCODED_LIMIT, ImageCache, ImageKey, ImageState, decode};
use gpui::{App, RenderImage};
use std::{
    collections::{HashSet, VecDeque},
    io::Read,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use ureq::Agent;
use url::{Host, Url};

/// Number of concurrent network/decoder workers across native windows.
const WORKERS: usize = 4;
/// Maximum distinct remote images waiting to be fetched.
const QUEUE_LIMIT: usize = 128;
/// Maximum recent HTTP or decoding failures retained across document changes.
const FAILURE_LIMIT: usize = 256;
/// Recent errors stay visible during relayouts before another fetch can be attempted.
const RETRY_DELAY: Duration = Duration::from_secs(60);
/// Deadline for response headers and body, including redirect hops.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// One connection pool respects the system proxy environment for remote hosts.
static PROXIED: LazyLock<Agent> = LazyLock::new(|| agent(false));
/// Loopback requests connect directly to the local service.
static DIRECT: LazyLock<Agent> = LazyLock::new(|| agent(true));

/// Remote image lifecycle on the UI thread. Its rasters live in the shared image cache.
#[derive(Default)]
pub(super) struct Requests {
    /// Changes after each completed fetch, invalidating layouts with pending images.
    pub(super) revision: u64,
    /// Deduplicated destinations waiting for a worker.
    queue: VecDeque<Url>,
    /// Destinations currently being fetched or decoded.
    running: HashSet<Url>,
    /// Bounded cache of failed destinations and the time of their last attempt.
    failures: VecDeque<(Url, Instant)>,
}

impl Requests {
    /// Queue `url` if needed, returning the state available before the fetch runs.
    fn enqueue(&mut self, url: Url) -> ImageState {
        if self.running.contains(&url) || self.queue.contains(&url) {
            return ImageState::Pending;
        }
        if let Some(index) = self.failures.iter().position(|(failed, _)| *failed == url) {
            if self.failures[index].1.elapsed() < RETRY_DELAY {
                return ImageState::Failed;
            }
            self.failures.remove(index);
        }
        if self.queue.len() >= QUEUE_LIMIT {
            return ImageState::Failed;
        }
        self.queue.push_back(url);
        ImageState::Pending
    }

    /// Claim the next destination if a worker is available.
    fn start_next(&mut self) -> Option<Url> {
        if self.running.len() >= WORKERS {
            return None;
        }
        let url = self.queue.pop_front()?;
        self.running.insert(url.clone());
        Some(url)
    }

    /// Settle one request for `url` and invalidate layouts with its prior state.
    fn settle(&mut self, url: Url, ready: bool) {
        self.running.remove(&url);
        if !ready {
            if self.failures.len() == FAILURE_LIMIT {
                self.failures.pop_front();
            }
            self.failures.push_back((url, Instant::now()));
        }
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Return a normalized HTTP(S) destination from `source`, without embedded credentials or fragments.
pub(super) fn request_url(source: &str) -> Option<Url> {
    let mut url = Url::parse(source).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

/// Reuse a ready raster or schedule a request for `url` through `cx`.
pub(super) fn request(url: Url, cx: &mut App) -> ImageState {
    let state = {
        let mut cache = cx.global::<ImageCache>().0.borrow_mut();
        if let Some(image) = cache.get(&ImageKey::Remote(url.clone())) {
            return ImageState::Ready(image);
        }
        cache.remote.enqueue(url)
    };
    start_pending(cx);
    state
}

/// Start outstanding jobs through `cx` until all background slots are occupied.
fn start_pending(cx: &mut App) {
    loop {
        let next = cx.global::<ImageCache>().0.borrow_mut().remote.start_next();
        let Some(url) = next else { break };
        let request = cx.background_executor().spawn(async move {
            let image = download(&url);
            (url, image)
        });
        cx.spawn(async move |cx| {
            let (url, image) = request.await;
            cx.update(|cx| finish(url, image, cx));
        })
        .detach();
    }
}

/// Publish the `image` downloaded from `url`, refresh measuring views and start queued work in `cx`.
fn finish(url: Url, image: Option<(RenderImage, usize)>, cx: &mut App) {
    let retired = {
        let mut cache = cx.global::<ImageCache>().0.borrow_mut();
        let (ready, retired) = image.map_or((false, Vec::new()), |(image, bytes)| {
            cache.insert(ImageKey::Remote(url.clone()), Arc::new(image), bytes)
        });
        cache.remote.settle(url, ready);
        retired
    };
    for image in retired {
        cx.drop_image(image, None);
    }
    cx.refresh_windows();
    start_pending(cx);
}

/// Open an HTTP connection pool with a bounded lifetime per request.
/// Set `direct` to bypass environment proxies on loopback requests.
fn agent(direct: bool) -> Agent {
    let mut config = Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(5);
    if direct {
        config = config.proxy(None);
    }
    Agent::new_with_config(config.build())
}

/// Read and decode the first raster frame from `url` on a background worker.
fn download(url: &Url) -> Option<(RenderImage, usize)> {
    let direct = match url.host()? {
        Host::Domain(host) => host.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
        Host::Ipv4(host) => host.is_loopback(),
        Host::Ipv6(host) => host.is_loopback(),
    };
    let agent = if direct { &*DIRECT } else { &*PROXIED };
    let mut response = agent
        .get(url.as_str())
        .header(
            "Accept",
            "image/png,image/jpeg,image/gif,image/webp,*/*;q=0.1",
        )
        .call()
        .ok()?;
    if !response.status().is_success()
        || response
            .body()
            .content_length()
            .is_some_and(|length| length > ENCODED_LIMIT)
    {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(ENCODED_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reflow and URL fragments share a fetch until the request settles.
    #[test]
    fn remote_images_deduplicate_and_remember_failures() {
        let url = request_url("https://example.com/image.png#one").unwrap();
        let same = request_url("HTTPS://example.com:443/image.png#two").unwrap();
        assert_eq!(url, same);
        assert!(request_url("file:///tmp/image.png").is_none());
        assert!(request_url("https://user:secret@example.com/image.png").is_none());
        let mut state = Requests::default();
        assert!(matches!(state.enqueue(url.clone()), ImageState::Pending));
        assert!(matches!(state.enqueue(same.clone()), ImageState::Pending));
        assert_eq!(state.start_next().as_ref(), Some(&url));
        assert!(matches!(state.enqueue(same.clone()), ImageState::Pending));
        assert!(state.start_next().is_none());
        state.settle(url, false);
        assert_eq!(state.revision, 1);
        assert!(matches!(state.enqueue(same), ImageState::Failed));
        assert!(state.start_next().is_none());
    }
}
