//! Movie decoding for plugins — the decoder half of `layerExMovie`.
//!
//! Part of [`crate::plugin_api`]: a plugin crate's production dependencies are
//! `krkr-engine` and `krkr-tjs2` only, so the vocabulary a movie player has to
//! name — krkr-video's [`VideoSource`], [`VideoDecoder`], [`VideoPort`],
//! [`VideoFrame`] and their errors — is re-exported here, and [`open_movie`]
//! is the one call that turns a project storage name into an open decoder.
//!
//! **The engine owns backend selection**, exactly as it does for
//! `VideoOverlay`: the host's `video_factory()` (`host.rs:653`) is the
//! capability a platform shell installed — `PlatformVideoFactory` on the
//! native shells, a DOM bridge on Web, `UnavailableVideoFactory` when nothing
//! was installed — and a plugin never picks an OS decoder itself. The movie's
//! bytes are read through the project storage (XP3 members included) and
//! handed over as [`VideoSource::Bytes`], the same flow `VideoOverlay` uses
//! and the counterpart of the reference plugin copying the storage stream
//! through `TVPCreateIStream` (`layerExMovie.cpp:142-167`).
//!
//! ```ignore
//! use krkr_engine::plugin_api::video;
//!
//! fn play(runtime: &mut Runtime<KrkrHost>, storage: &str) -> Result<()> {
//!     let mut decoder = video::open_movie(runtime, storage)?;
//!     let metadata = decoder.metadata().clone();
//!     while let Some(frame) = decoder.next_frame()? {
//!         // `frame.width` x `frame.height` RGBA, `frame.stride` bytes per row.
//!     }
//!     Ok(())
//! }
//! ```
//!
//! **Thread and ownership contract.** [`open_movie`] runs on the script thread
//! and hands the *plugin* the decoder; the engine keeps no handle on it, and
//! the plugin drops it to release the movie's resources. A decoder is `Send`
//! (`VideoDecoder: Send`), so a plugin may run it on its own worker thread,
//! but only the script thread may turn a frame into pixels — the layer views
//! in [`crate::plugin_api::layer`] are reached through `&mut Runtime<KrkrHost>`
//! and never cross a thread. The engine's own `VideoOverlay` takes the same
//! decoder type onto a dedicated decode thread with a bounded queue;
//! `layerExMovie` pulls frames from a `System.addContinuousHandler` callback
//! instead.
//!
//! **Errors.** [`VideoOpenError`] keeps the two failures apart — a movie the
//! storage could not hand out ([`VideoOpenError::Storage`]) and a backend
//! that refused the bytes it was given ([`VideoOpenError::Backend`]) — which
//! is how `layerExMovie::openMovie` reports them (`:142-147` logs its own read
//! failure and returns, `:180-184` returns when its stream fails to open), and
//! it converts to [`TjsError`] for a plugin that throws instead of logging. No
//! capability check stands in the way: a host whose backend cannot decode
//! in-memory bytes reports [`VideoError::Unsupported`] from the factory
//! itself, which is the one place that knows.
//!
//! Reference line numbers refer to the krkrz checkout at
//! `/Users/ruri/repo/krkrz` (`last_hodgepodge_repository`, Shift-JIS sources
//! converted with `iconv -f CP932`), re-read one by one on 2026-09-12:
//! `layerExMovie.cpp` is 421 lines, `main.cpp` 61.

use std::{error::Error, fmt};

use krkr_tjs2::{TjsError, runtime::Runtime};

use crate::host::KrkrHost;

pub use krkr_video::{
    AudioChunk, AudioSpec, VideoBackendKind, VideoCapabilities, VideoDecoder, VideoDecoderFactory,
    VideoError, VideoFrame, VideoMetadata, VideoPort, VideoSource,
};

/// Why [`open_movie`] could not produce a decoder.
#[derive(Debug)]
pub enum VideoOpenError {
    /// The project storage has no readable movie under this name — the
    /// reference's `TVPCreateIStream` failure (`layerExMovie.cpp:142-147`).
    Storage {
        /// The name the plugin asked for, as the script spelled it.
        name: String,
        /// What the storage reported (a missing file, an unreadable archive
        /// member, a deferred Web asset).
        message: String,
    },
    /// The host's decoder backend refused the bytes it was handed.
    Backend(VideoError),
}

impl fmt::Display for VideoOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoOpenError::Storage { name, message } => write!(f, "{name}: {message}"),
            VideoOpenError::Backend(error) => write!(f, "{error}"),
        }
    }
}

impl Error for VideoOpenError {}

impl From<VideoOpenError> for TjsError {
    fn from(error: VideoOpenError) -> Self {
        TjsError::runtime(error.to_string())
    }
}

/// Opens `storage` as a movie and returns the host-backed decoder.
///
/// The bytes are read through the project storage — a movie inside an XP3
/// archive opens exactly like a loose file, and the whole movie is held in
/// memory, the way `VideoOverlay` opens one and the reference plugin copies
/// its stream. The storage name travels along as the [`VideoSource`] name
/// hint, because a system framework that sniffs the container may key on the
/// extension. The decoder belongs to the caller from here on: the plugin pumps
/// it (see the module docs for the per-frame loop the plugin-facing facilities
/// make possible) and drops it to release the movie.
pub fn open_movie(
    runtime: &mut Runtime<KrkrHost>,
    storage: &str,
) -> std::result::Result<Box<dyn VideoPort>, VideoOpenError> {
    let bytes = runtime
        .host_mut()
        .read_binary_storage_for_tjs(storage)
        .map_err(|error| VideoOpenError::Storage {
            name: storage.to_string(),
            message: error.message,
        })?;
    runtime
        .host()
        .video_factory()
        .create(VideoSource::bytes(bytes, Some(storage)))
        .map_err(VideoOpenError::Backend)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use krkr_assets::ProjectStorage;
    use krkr_tjs2::runtime::Runtime;

    use crate::{EngineConfig, KrkrEngine, KrkrHost, SystemPaths};

    use super::*;

    /// Records what the helper handed the backend, so a test can see both the
    /// bytes and the name hint a framework may sniff the container with.
    #[derive(Default)]
    struct RecordingFactory {
        sources: Mutex<Vec<VideoSource>>,
    }

    /// A decoder with no frames: enough to see what reaches the backend.
    struct NoopPort;

    impl VideoDecoder for NoopPort {
        fn metadata(&self) -> &VideoMetadata {
            static METADATA: VideoMetadata = VideoMetadata {
                width: 2,
                height: 1,
                fps: 30.0,
                frame_count: 1,
                duration_ms: 33,
                has_audio: false,
            };
            &METADATA
        }

        fn next_frame(&mut self) -> std::result::Result<Option<VideoFrame>, VideoError> {
            Ok(None)
        }

        fn seek_ms(&mut self, _ms: i64) -> std::result::Result<(), VideoError> {
            Ok(())
        }
    }

    impl VideoPort for NoopPort {}

    impl VideoDecoderFactory for RecordingFactory {
        fn capabilities(&self) -> VideoCapabilities {
            VideoCapabilities::unavailable()
        }

        fn create(
            &self,
            source: VideoSource,
        ) -> std::result::Result<Box<dyn VideoPort>, VideoError> {
            self.sources.lock().expect("lock").push(source);
            Ok(Box::new(NoopPort))
        }
    }

    /// An engine over an in-memory project holding `movie.mp4`, decoding
    /// through `factory` — the shape a platform shell installs.
    fn engine(factory: Arc<dyn VideoDecoderFactory>) -> KrkrEngine {
        let storage = ProjectStorage::from_memory([("movie.mp4", b"not really a movie".to_vec())]);
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            video_factory: factory,
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    /// The failure `open_movie` reported: a decoder is not `Debug`, so
    /// `expect_err` cannot carry it.
    fn open_failure(runtime: &mut Runtime<KrkrHost>, storage: &str) -> VideoOpenError {
        match open_movie(runtime, storage) {
            Ok(_) => panic!("{storage} was not supposed to open"),
            Err(error) => error,
        }
    }

    /// The helper reads the storage bytes and passes them on with the name
    /// hint, the way `VideoOverlay` opens a movie.
    #[test]
    fn open_movie_reads_the_storage_and_hands_the_factory_its_bytes() {
        let recorder = Arc::new(RecordingFactory::default());
        let factory: Arc<dyn VideoDecoderFactory> = recorder.clone();
        let mut engine = engine(factory);
        let decoder = open_movie(engine.tjs_runtime_mut(), "movie.mp4").expect("open");
        assert_eq!(decoder.metadata().width, 2);
        assert!(decoder.audio_spec().is_none());

        let sources = recorder.sources.lock().expect("lock");
        let [VideoSource::Bytes { data, name }] = &sources[..] else {
            panic!("one bytes source, got {sources:?}");
        };
        assert_eq!(data.as_ref(), b"not really a movie");
        assert_eq!(name.as_deref(), Some("movie.mp4"));
    }

    /// A name the storage cannot resolve is the reference's read failure
    /// (`layerExMovie.cpp:142-147`), not a backend refusal.
    #[test]
    fn a_missing_storage_reports_the_name_it_could_not_read() {
        let recorder = Arc::new(RecordingFactory::default());
        let factory: Arc<dyn VideoDecoderFactory> = recorder.clone();
        let mut engine = engine(factory);
        let error = open_failure(engine.tjs_runtime_mut(), "missing.mp4");
        match &error {
            VideoOpenError::Storage { name, message } => {
                assert_eq!(name, "missing.mp4");
                assert!(!message.is_empty(), "the storage's own reason survives");
            }
            other => panic!("expected a storage failure, got {other:?}"),
        }
        assert!(error.to_string().contains("missing.mp4"));
        assert_eq!(
            recorder.sources.lock().expect("lock").len(),
            0,
            "nothing was handed to the backend"
        );
        let raised: TjsError = error.into();
        assert_eq!(raised.kind, krkr_tjs2::TjsErrorKind::Runtime);
    }

    /// A host shell that installed no backend is a host whose movies cannot
    /// open, even with the bytes in hand.
    #[test]
    fn a_host_without_a_backend_reports_unsupported() {
        let mut engine = engine(Arc::new(krkr_video::UnavailableVideoFactory));
        let error = open_failure(engine.tjs_runtime_mut(), "movie.mp4");
        assert!(
            matches!(&error, VideoOpenError::Backend(VideoError::Unsupported(_))),
            "got {error:?}"
        );
    }

    /// A host that only runs scripts has no storage to read a movie from.
    #[test]
    fn a_host_without_storage_reports_the_storage_failure() {
        let recorder = Arc::new(RecordingFactory::default());
        let factory: Arc<dyn VideoDecoderFactory> = recorder.clone();
        let host = KrkrHost::with_system_paths_and_video_factory(SystemPaths::default(), factory);
        let mut runtime: Runtime<KrkrHost> = Runtime::with_host(host);
        let error = open_failure(&mut runtime, "movie.mp4");
        assert!(matches!(error, VideoOpenError::Storage { .. }), "{error:?}");
        assert!(
            recorder.sources.lock().expect("lock").is_empty(),
            "a movie that never opened never reached the backend"
        );
    }
}
