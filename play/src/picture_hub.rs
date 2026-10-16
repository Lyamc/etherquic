//! Which picture the window is showing.
//!
//! Several player sessions can be open. The window thread draws the shown
//! seat and can rebind without waiting for another session to finish.
//! `connect_view` waits in [`hold_picture`], which returns when that seat
//! stops even if a different seat still owns the window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::outbox::Gate;
use crate::{LiveView, Picture};

struct Seat {
    id: u64,
    label: String,
    frames: Arc<Mutex<Option<Picture>>>,
    input: Arc<Gate>,
    stop: Arc<AtomicBool>,
    trace: bool,
    view: LiveView,
    stamps: Arc<Mutex<std::collections::HashMap<u64, std::time::Instant>>>,
    title: Arc<Mutex<String>>,
    cursor: Arc<Mutex<etherquic::cursor::CursorView>>,
    done: Arc<(Mutex<bool>, Condvar)>,
}

struct Inner {
    seats: Vec<Seat>,
    shown: Option<u64>,
    next: u64,
}

struct Hub {
    inner: Mutex<Inner>,
    cv: Condvar,
    waker: Mutex<Option<Box<dyn Fn() + Send>>>,
}

fn hub() -> &'static Hub {
    static HUB: std::sync::OnceLock<Hub> = std::sync::OnceLock::new();
    HUB.get_or_init(|| Hub {
        inner: Mutex::new(Inner {
            seats: Vec::new(),
            shown: None,
            next: 1,
        }),
        cv: Condvar::new(),
        waker: Mutex::new(None),
    })
}

fn lock_inner() -> std::sync::MutexGuard<'static, Inner> {
    hub()
        .inner
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// What the window binds when it draws.
#[derive(Clone)]
pub struct PictureSeat {
    pub id: u64,
    pub frames: Arc<Mutex<Option<Picture>>>,
    pub input: Arc<Gate>,
    pub stop: Arc<AtomicBool>,
    pub trace: bool,
    pub view: LiveView,
    pub stamps: Arc<Mutex<std::collections::HashMap<u64, std::time::Instant>>>,
    pub title: Arc<Mutex<String>>,
    pub cursor: Arc<Mutex<etherquic::cursor::CursorView>>,
}

/// Wake the picture window after the hub lock is released.
pub fn set_picture_waker(waker: Box<dyn Fn() + Send>) {
    *hub()
        .waker
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(waker);
}

pub(crate) fn wake_picture() {
    wake();
}

fn wake() {
    // The waker sends a window event. It must not lock the hub.
    if let Some(waker) = hub()
        .waker
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
    {
        waker();
    }
    hub().cv.notify_all();
}

/// Open a picture and make it the one on screen. The others stay open.
pub fn open_picture(label: impl Into<String>, seat: PictureSeat) -> u64 {
    let id = {
        let mut inner = lock_inner();
        let id = inner.next;
        inner.next += 1;
        inner.seats.push(Seat {
            id,
            label: label.into(),
            frames: seat.frames,
            input: seat.input,
            stop: seat.stop,
            trace: seat.trace,
            view: seat.view,
            stamps: seat.stamps,
            title: seat.title,
            cursor: seat.cursor,
            done: Arc::new((Mutex::new(false), Condvar::new())),
        });
        inner.shown = Some(id);
        id
    };
    wake();
    id
}

/// Make `id` the picture on screen. The others stay open.
///
/// `0` hides the window and leaves every session open.
pub fn switch_picture(id: u64) -> bool {
    let switched = {
        let mut inner = lock_inner();
        if id == 0 {
            inner.shown = None;
            true
        } else if inner.seats.iter().any(|seat| seat.id == id) {
            inner.shown = Some(id);
            true
        } else {
            false
        }
    };
    if switched {
        wake();
    }
    switched
}

pub fn shown_picture() -> Option<u64> {
    lock_inner().shown
}

pub fn picture_open(id: u64) -> bool {
    lock_inner().seats.iter().any(|seat| seat.id == id)
}

/// The seat the window should draw. `None` when the picture is hidden.
pub fn screen_seat() -> Option<PictureSeat> {
    let inner = lock_inner();
    let id = inner.shown?;
    inner.seats.iter().find(|seat| seat.id == id).map(seat_view)
}

fn seat_view(seat: &Seat) -> PictureSeat {
    PictureSeat {
        id: seat.id,
        frames: Arc::clone(&seat.frames),
        input: Arc::clone(&seat.input),
        stop: Arc::clone(&seat.stop),
        trace: seat.trace,
        view: Arc::clone(&seat.view),
        stamps: Arc::clone(&seat.stamps),
        title: Arc::clone(&seat.title),
        cursor: Arc::clone(&seat.cursor),
    }
}

/// Close one picture. When it was on screen, another open one becomes shown.
/// This does not wait for the window thread. The window thread is woken so it
/// can follow the change.
pub fn close_picture(id: u64) -> bool {
    close_seat(id, true)
}

/// Same as [`close_picture`] without posting a wake. The window thread uses
/// this while it is already inside its event handler, so a wake cannot be
/// left queued after the handler asks the loop to exit.
pub(crate) fn close_picture_quietly(id: u64) -> bool {
    close_seat(id, false)
}

fn close_seat(id: u64, wake_window: bool) -> bool {
    let done = {
        let mut inner = lock_inner();
        let Some(pos) = inner.seats.iter().position(|seat| seat.id == id) else {
            return false;
        };
        let was_shown = inner.shown == Some(id);
        let seat = inner.seats.remove(pos);
        seat.stop.store(true, Ordering::SeqCst);
        if was_shown {
            inner.shown = inner
                .seats
                .get(pos)
                .or_else(|| inner.seats.last())
                .map(|seat| seat.id);
        }
        seat.done
    };
    {
        let (lock, cv) = &*done;
        let mut finished = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        *finished = true;
        cv.notify_all();
    }
    if wake_window {
        wake();
    }
    true
}

/// Block until this picture is closed or `stop` is set.
///
/// A different picture can still be inside the window thread. Setting `stop`
/// returns here without joining that thread.
pub fn hold_picture(id: u64, stop: &AtomicBool) {
    let done = {
        let inner = lock_inner();
        inner
            .seats
            .iter()
            .find(|seat| seat.id == id)
            .map(|seat| Arc::clone(&seat.done))
    };
    let Some(done) = done else {
        return;
    };
    let (lock, cv) = &*done;
    let mut finished = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    while !*finished && !stop.load(Ordering::SeqCst) {
        let (guard, _) = cv
            .wait_timeout(finished, Duration::from_millis(30))
            .unwrap_or_else(|poison| poison.into_inner());
        finished = guard;
    }
    drop(finished);
    let _ = close_picture(id);
}

/// Park until some picture should be on screen.
pub fn wait_for_picture() {
    let mut inner = lock_inner();
    while inner.shown.is_none() {
        inner = hub()
            .cv
            .wait(inner)
            .unwrap_or_else(|poison| poison.into_inner());
    }
}

#[cfg(test)]
fn reset_pictures() {
    let mut inner = lock_inner();
    inner.seats.clear();
    inner.shown = None;
    inner.next = 1;
}

#[cfg(test)]
fn test_seat(stop: &Arc<AtomicBool>) -> PictureSeat {
    PictureSeat {
        id: 0,
        frames: Arc::new(Mutex::new(None)),
        input: Arc::new(Gate::new()),
        stop: Arc::clone(stop),
        trace: false,
        view: crate::live(crate::ViewOptions::default()),
        stamps: Arc::new(Mutex::new(std::collections::HashMap::new())),
        title: Arc::new(Mutex::new(String::new())),
        cursor: etherquic::cursor::share(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn two_sessions_switch_and_the_closed_one_does_not_wait_on_the_window() {
        reset_pictures();
        let stop_a = Arc::new(AtomicBool::new(false));
        let stop_b = Arc::new(AtomicBool::new(false));
        let frames_a = Arc::new(Mutex::new(Some(Picture {
            width: 1,
            height: 1,
            rgb: vec![1, 0, 0],
        })));
        let frames_b = Arc::new(Mutex::new(Some(Picture {
            width: 1,
            height: 1,
            rgb: vec![0, 0, 2],
        })));
        let mut seat_a = test_seat(&stop_a);
        seat_a.frames = Arc::clone(&frames_a);
        let mut seat_b = test_seat(&stop_b);
        seat_b.frames = Arc::clone(&frames_b);
        let first = open_picture("one", seat_a);
        let second = open_picture("two", seat_b);
        assert_eq!(shown_picture(), Some(second));
        assert!(picture_open(first));
        assert!(picture_open(second));

        assert!(switch_picture(first));
        assert_eq!(shown_picture(), Some(first));
        assert!(picture_open(second));
        let shown = screen_seat().expect("first picture");
        assert_eq!(shown.id, first);
        assert!(Arc::ptr_eq(&shown.frames, &frames_a));

        assert!(switch_picture(second));
        let shown = screen_seat().expect("second picture");
        assert_eq!(shown.id, second);
        assert!(Arc::ptr_eq(&shown.frames, &frames_b));
        assert!(picture_open(first));

        // The window thread is the thing stuck inside run_app for the shown
        // seat. Closing the other seat must not join it.
        let window_stop = Arc::clone(&stop_b);
        let window = thread::spawn(move || {
            while picture_open(second) && shown_picture() == Some(second) {
                if window_stop.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let stop = Arc::clone(&stop_a);
        let started = Instant::now();
        let waiter = thread::spawn(move || hold_picture(first, &stop));
        thread::sleep(Duration::from_millis(80));
        assert!(!waiter.is_finished());
        stop_a.store(true, Ordering::SeqCst);
        waiter.join().expect("waiter");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            !window.is_finished(),
            "closing the other picture joined the window waiter"
        );
        assert!(!picture_open(first));
        assert!(picture_open(second));
        assert_eq!(shown_picture(), Some(second));
        let shown = screen_seat().expect("second stays on screen");
        assert!(Arc::ptr_eq(&shown.frames, &frames_b));

        assert!(close_picture(second));
        window.join().expect("window");
        assert!(shown_picture().is_none());
        assert!(!picture_open(second));
    }
}
