// Minimal FSEvents binding: one recursive stream per checkout, callbacks on a
// global dispatch queue. Only the handful of CoreServices entry points needed
// are declared; CoreFoundation values come from objc2-core-foundation.
use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{CFArray, CFRetained, CFString};

type StreamRef = *mut c_void;
type Callback = unsafe extern "C" fn(
    stream: *const c_void,
    info: *mut c_void,
    num_events: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
}

const SINCE_NOW: u64 = u64::MAX;
const FLAG_NO_DEFER: u32 = 0x2;
const QOS_CLASS_UTILITY: isize = 0x11;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: Callback,
        context: *mut Context,
        paths: *const c_void,
        since_when: u64,
        latency: f64,
        flags: u32,
    ) -> StreamRef;
    fn FSEventStreamSetDispatchQueue(stream: StreamRef, queue: *mut c_void);
    fn FSEventStreamStart(stream: StreamRef) -> u8;
    fn FSEventStreamStop(stream: StreamRef);
    fn FSEventStreamInvalidate(stream: StreamRef);
    fn FSEventStreamRelease(stream: StreamRef);
}

unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
}

type Handler = Box<dyn Fn() + Send + Sync>;

unsafe extern "C" fn on_events(
    _stream: *const c_void,
    info: *mut c_void,
    _num_events: usize,
    _paths: *mut c_void,
    _flags: *const u32,
    _ids: *const u64,
) {
    // SAFETY: `info` is the leaked `Box<Handler>` from `Stream::new`, alive
    // until the release callback runs after `FSEventStreamRelease`.
    let handler = unsafe { &*info.cast::<Handler>() };
    handler();
}

unsafe extern "C" fn release_handler(info: *const c_void) {
    // SAFETY: called exactly once by FSEvents when the stream is released.
    drop(unsafe { Box::from_raw(info.cast_mut().cast::<Handler>()) });
}

/// A running recursive watch over `paths`; stops when dropped.
pub struct Stream(StreamRef);

impl Stream {
    pub fn new(
        paths: &[&Path],
        latency: f64,
        handler: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let strings: Vec<CFRetained<CFString>> = paths
            .iter()
            .map(|path| CFString::from_str(&path.to_string_lossy()))
            .collect();
        let array = CFArray::from_retained_objects(&strings);
        let handler: Handler = Box::new(handler);
        let mut context = Context {
            version: 0,
            info: Box::into_raw(Box::new(handler)).cast(),
            retain: None,
            release: Some(release_handler),
            copy_description: None,
        };
        // SAFETY: all pointers outlive the call; FSEvents retains the path
        // array and owns the handler through the release callback.
        let stream = unsafe {
            FSEventStreamCreate(
                std::ptr::null(),
                on_events,
                &mut context,
                (&*array as *const CFArray<CFString>).cast(),
                SINCE_NOW,
                latency,
                FLAG_NO_DEFER,
            )
        };
        if stream.is_null() {
            // SAFETY: FSEvents never took ownership of the handler.
            drop(unsafe { Box::from_raw(context.info.cast::<Handler>()) });
            return Err(format!("FSEventStreamCreate failed for {paths:?}"));
        }
        // SAFETY: `stream` is valid and not yet started.
        unsafe {
            FSEventStreamSetDispatchQueue(stream, dispatch_get_global_queue(QOS_CLASS_UTILITY, 0));
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                return Err(format!("FSEventStreamStart failed for {paths:?}"));
            }
        }
        Ok(Self(stream))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream was started in `new` and is released exactly once.
        unsafe {
            FSEventStreamStop(self.0);
            FSEventStreamInvalidate(self.0);
            FSEventStreamRelease(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn reports_changes_under_watched_directory() {
        let dir = tempfile::tempdir().unwrap();
        let watched = dir.path().canonicalize().unwrap();
        let (sender, receiver) = mpsc::channel();
        let stream = Stream::new(&[&watched], 0.05, move || {
            let _ = sender.send(());
        })
        .unwrap();
        std::fs::create_dir(watched.join("nested")).unwrap();
        std::fs::write(watched.join("nested/file"), "x").unwrap();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("change notification");
        drop(stream);
    }
}
