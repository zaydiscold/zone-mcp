//! The flash receiver behind the local page, on a thread of its own.
//!
//! The page posts camera pictures as fast as the camera takes them, and asks where the flash
//! stands. Most pictures take a few milliseconds to take in. A few things take longer: putting
//! a large flash together once its last piece arrives, setting a misread code aside, and
//! storing a large library. If those ran while a request held the receiver, every other
//! picture and every status request would wait behind them, and the page, hearing nothing,
//! would say the phone was out of sight while zone was in fact busy with it.
//!
//! So one thread owns the [`Receiver`] and does all of that work, and requests only hand it
//! codes and read what it last said. A request that hands in a picture waits for the picture
//! to be taken in (a few milliseconds) but never longer than [`WAIT`]; past that it answers
//! with where the flash stood, saying what zone is busy with (`work`: `solving` while the
//! pieces are put together, `storing` while the library takes the items), and the state
//! `storing`, so the page can say "putting your library together" and mean it. Pictures that
//! arrive meanwhile wait their turn, at most [`MAX_WAITING`] of them: past that the oldest
//! waiting one is dropped, which costs nothing, as the phone shows its codes again and again.
//!
//! Starting over cancels at once a flash being put together, and the search for a misread code
//! when it is making a flash's rows again from every reading.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::fountain;
use super::receive::{self, Outcome, PictureRead, Receiver, Step};
use crate::db::Db;
use crate::{info, paths, warn};

/// The longest a request waits for its codes to be taken in before answering.
pub const WAIT: Duration = Duration::from_secs(1);
/// Pictures (or frames) waiting their turn, at most.
pub const MAX_WAITING: usize = 64;
/// A flash put together more slowly than this says so in the log.
const SLOW: Duration = Duration::from_secs(1);

/// The receiver's thread, as the page's routes see it.
pub struct FlashWorker {
    inner: Arc<Inner>,
    wait: Duration,
}

struct Inner {
    shared: Mutex<Shared>,
    signal: Condvar,
    /// Asks the thread to stop putting a flash together: the page started over, or zone is
    /// closing.
    cancel: AtomicBool,
    /// For tests: while closed, the thread waits just before putting a flash together.
    #[cfg(test)]
    gate: (Mutex<bool>, Condvar),
    /// For tests: while held (the first), the thread waits the next time it asks whether to
    /// stop a long step of taking a code in, and says it got there (the second).
    #[cfg(test)]
    hold: (Mutex<(bool, bool)>, Condvar),
    /// For tests: how many times the thread asked whether to stop a long step.
    #[cfg(test)]
    asked: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct Shared {
    jobs: VecDeque<(u64, Job)>,
    /// The number of the last job handed in, and of the last one finished. Jobs are finished
    /// in order, so every job up to `finished` is done (or was dropped).
    handed_in: u64,
    finished: u64,
    /// Where the flash stood when the thread last said, and what it is busy with.
    status: Value,
    /// When a picture last held a code of any kind.
    last_code_at: Option<Instant>,
    /// Pictures dropped because too many were waiting.
    dropped: u64,
    /// The page's setup line as last read, for when the library is busy.
    setup: Option<Value>,
    closing: bool,
}

enum Job {
    Picture { codes: Vec<Vec<u8>>, color: bool },
    Frame(Vec<u8>),
    Reset,
}

fn lock(inner: &Inner) -> MutexGuard<'_, Shared> {
    inner.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FlashWorker {
    /// Starts the thread for the library `db`, which already knows the flashes it stored.
    pub fn start(db: Arc<Mutex<Db>>) -> FlashWorker {
        FlashWorker::start_waiting(db, WAIT)
    }

    /// [`FlashWorker::start`] with another longest wait (tests wait longer, to be exact).
    pub fn start_waiting(db: Arc<Mutex<Db>>, wait: Duration) -> FlashWorker {
        let stored = match db.lock() {
            Ok(db) => db.stored_flashes().unwrap_or_else(|error| {
                warn!("flash: could not read which flashes were stored before: {error:#}");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        let receiver = Receiver::with_stored(stored);
        let inner = Arc::new(Inner {
            shared: Mutex::new(Shared { status: with_work(receiver.status(), None), ..Shared::default() }),
            signal: Condvar::new(),
            cancel: AtomicBool::new(false),
            #[cfg(test)]
            gate: (Mutex::new(false), Condvar::new()),
            #[cfg(test)]
            hold: (Mutex::new((false, false)), Condvar::new()),
            #[cfg(test)]
            asked: std::sync::atomic::AtomicUsize::new(0),
        });
        let thread = inner.clone();
        if let Err(error) = std::thread::Builder::new().name("zone-flash".into()).spawn(move || run(&thread, &db, receiver)) {
            warn!("flash: could not start the flash thread: {error}");
            lock(&inner).closing = true;
        }
        FlashWorker { inner, wait }
    }

    /// Whether a code was in view in the last few seconds: then a picture that seems to hold
    /// none is worth a slower, second look.
    pub fn codes_in_view(&self) -> bool {
        lock(&self.inner).last_code_at.is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    }

    /// Hands in the codes read from one camera picture. Answers with where the flash stands.
    pub fn picture(&self, codes: Vec<Vec<u8>>, color: bool) -> Value {
        let read = PictureRead { codes: codes.len(), frames: codes.iter().filter(|code| fountain::parse(code).is_ok()).count(), useful: 0, color };
        let mut shared = lock(&self.inner);
        if !codes.is_empty() {
            shared.last_code_at = Some(Instant::now());
        }
        let ticket = self.hand_in(&mut shared, Job::Picture { codes, color });
        let (shared, done) = self.wait_for(shared, ticket);
        let mut status = shared.status.clone();
        if !done {
            // Not taken in yet: say at least what the picture held, so the page knows the
            // phone is in view.
            status["last"] = json!({ "codes": read.codes, "frames": read.frames, "useful": 0, "color": read.color });
        }
        status
    }

    /// Hands in the bytes of one frame.
    pub fn frame(&self, raw: Vec<u8>) -> Value {
        let mut shared = lock(&self.inner);
        let ticket = self.hand_in(&mut shared, Job::Frame(raw));
        self.wait_for(shared, ticket).0.status.clone()
    }

    /// Starts over: drops what waits, stops a flash being put together, and forgets every
    /// flash but the ones stored.
    pub fn reset(&self) -> Value {
        let mut shared = lock(&self.inner);
        shared.jobs.clear();
        self.inner.cancel.store(true, Ordering::Relaxed);
        let ticket = self.hand_in(&mut shared, Job::Reset);
        self.wait_for(shared, ticket).0.status.clone()
    }

    /// Where the flash stands, as the thread last said.
    pub fn status(&self) -> Value {
        lock(&self.inner).status.clone()
    }

    /// Remembers the page's setup line (`fresh`), or gives the last one remembered.
    pub fn setup(&self, fresh: Option<Value>) -> Option<Value> {
        let mut shared = lock(&self.inner);
        if let Some(fresh) = fresh {
            shared.setup = Some(fresh);
        }
        shared.setup.clone()
    }

    fn hand_in(&self, shared: &mut Shared, job: Job) -> u64 {
        while shared.jobs.len() >= MAX_WAITING {
            let Some(oldest) = shared.jobs.iter().position(|(_, job)| !matches!(job, Job::Reset)) else { break };
            shared.jobs.remove(oldest);
            shared.dropped += 1;
        }
        shared.handed_in += 1;
        let ticket = shared.handed_in;
        shared.jobs.push_back((ticket, job));
        self.inner.signal.notify_all();
        ticket
    }

    /// Waits, at most [`WAIT`], until job `ticket` is done. True when it is.
    fn wait_for<'a>(&'a self, mut shared: MutexGuard<'a, Shared>, ticket: u64) -> (MutexGuard<'a, Shared>, bool) {
        let deadline = Instant::now() + self.wait;
        while shared.finished < ticket && !shared.closing {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            shared = self.inner.signal.wait_timeout(shared, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
        let done = shared.finished >= ticket;
        (shared, done)
    }
}

impl Drop for FlashWorker {
    fn drop(&mut self) {
        lock(&self.inner).closing = true;
        self.inner.cancel.store(true, Ordering::Relaxed);
        self.inner.signal.notify_all();
    }
}

/// The status with what the thread is busy with. While it is putting a flash together or
/// storing one, the state is `storing` whatever else is going on.
fn with_work(mut status: Value, work: Option<&str>) -> Value {
    if work.is_some() {
        status["state"] = json!("storing");
    }
    status["work"] = json!(work);
    status
}

fn publish(inner: &Inner, receiver: &Receiver, work: Option<&str>, finished: Option<u64>) {
    let status = with_work(receiver.status(), work);
    let mut shared = lock(inner);
    shared.status = status;
    if let Some(ticket) = finished {
        shared.finished = ticket;
    }
    inner.signal.notify_all();
}

fn run(inner: &Inner, db: &Mutex<Db>, mut receiver: Receiver) {
    loop {
        let (ticket, job) = {
            let mut shared = lock(inner);
            loop {
                if shared.closing {
                    return;
                }
                if let Some(next) = shared.jobs.pop_front() {
                    break next;
                }
                shared = inner.signal.wait(shared).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        match job {
            Job::Reset => {
                inner.cancel.store(false, Ordering::Relaxed);
                receiver.reset();
            }
            Job::Picture { codes, color } => {
                receiver.pictures += 1;
                if !codes.is_empty() {
                    receiver.pictures_with_code += 1;
                }
                if color {
                    receiver.color_pictures += 1;
                }
                let mut read = PictureRead { codes: codes.len(), color, ..PictureRead::default() };
                for code in &codes {
                    if inner.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let (frame, useful) = take(inner, db, &mut receiver, code);
                    read.frames += usize::from(frame);
                    read.useful += usize::from(useful);
                }
                receiver.last_picture = Some(read);
            }
            Job::Frame(raw) => {
                take(inner, db, &mut receiver, &raw);
            }
        }
        publish(inner, &receiver, None, Some(ticket));
    }
}

/// Whether to stop a long step of taking a code in, making a flash's rows again (see
/// `Assembler::take`): the page started over, or zone is closing.
fn stopping(inner: &Inner) -> bool {
    #[cfg(test)]
    {
        inner.asked.fetch_add(1, Ordering::Relaxed);
        let (held, released) = &inner.hold;
        let mut held = held.lock().unwrap();
        if held.0 {
            held.1 = true;
            released.notify_all();
            while held.0 {
                held = released.wait(held).unwrap();
            }
        }
    }
    inner.cancel.load(Ordering::Relaxed)
}

/// Takes the bytes of one frame. The frame that completes a flash puts it together and stores
/// it. Returns whether it was a flash frame, and whether it brought a new row.
fn take(inner: &Inner, db: &Mutex<Db>, receiver: &mut Receiver, raw: &[u8]) -> (bool, bool) {
    let color = receiver.color_pictures > 0;
    let (shape, payload) = match receiver.take(raw, &|| stopping(inner)) {
        Step::Ignored => return (false, false),
        Step::AlreadyDone => return (true, false),
        Step::Progress { useful } => return (true, useful),
        Step::GaveUp { reason, bytes } => {
            warn!("flash: gave up on a flash of {bytes} bytes: {reason}");
            if let Ok(db) = db.lock() {
                receive::record_events(&db, &Outcome::Failed(reason), bytes, color);
            }
            return (true, false);
        }
        Step::Ready { shape } => {
            publish(inner, receiver, Some("solving"), None);
            #[cfg(test)]
            {
                let (closed, opened) = &inner.gate;
                let mut closed = closed.lock().unwrap();
                while *closed {
                    closed = opened.wait(closed).unwrap();
                }
            }
            let started = Instant::now();
            let Some(payload) = receiver.solve(shape, &|| inner.cancel.load(Ordering::Relaxed)) else {
                return (true, true);
            };
            if started.elapsed() > SLOW {
                info!("flash: put {} pieces together in {:.1?}", shape.k, started.elapsed());
            }
            (shape, payload)
        }
        Step::Complete { shape, payload } => (shape, payload),
    };
    publish(inner, receiver, Some("storing"), None);
    let outcome = match db.lock() {
        Ok(mut db) => {
            let outcome = receive::import(&mut db, &shape, &payload, paths::now_unix());
            receive::record_events(&db, &outcome, payload.len(), color);
            outcome
        }
        Err(_) => Outcome::NotStored("The library is unavailable just now. Trying again.".to_string()),
    };
    match &outcome {
        Outcome::Stored(stored) => info!(
            "flash: {} sent {} items, {} new or changed{}{}",
            stored.phone_name.as_deref().unwrap_or("a phone"),
            stored.items,
            stored.inserted + stored.replaced,
            if stored.paired { ", and is now paired" } else { "" },
            if stored.check.all_checked() { String::new() } else { format!(". The check found a problem: {}", stored.check.problems.join(" ")) }
        ),
        Outcome::AlreadyStored(earlier) => info!("flash: a flash of {} items was stored before, so it was not stored again", earlier.items),
        Outcome::Failed(reason) => warn!("flash: a complete flash of {} bytes was not stored: {reason}", payload.len()),
        Outcome::NotOpened(_) => info!("flash: a complete flash of {} bytes did not open; reading more codes to check it", payload.len()),
        Outcome::NotStored(reason) => warn!("flash: a flash of {} bytes opened but was not stored yet: {reason}", payload.len()),
    }
    receiver.finish(shape, &payload, outcome);
    (true, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flash::fountain::Encoder;
    use crate::flash::seal;

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";

    impl FlashWorker {
        fn gate(&self, closed: bool) {
            let (lock, opened) = &self.inner.gate;
            *lock.lock().unwrap() = closed;
            opened.notify_all();
        }

        /// Holds the thread the next time it asks whether to stop a long step, or lets it go.
        fn hold(&self, held: bool) {
            let (lock, released) = &self.inner.hold;
            lock.lock().unwrap().0 = held;
            released.notify_all();
        }

        /// True once the thread is held there.
        fn is_held(&self) -> bool {
            self.inner.hold.0.lock().unwrap().1
        }
    }

    fn library() -> Arc<Mutex<Db>> {
        let db = Db::open_in_memory().unwrap();
        db.create_offer(SECRET, paths::now_unix(), 600.0).unwrap();
        Arc::new(Mutex::new(db))
    }

    fn flash(items: usize, session: u32) -> Encoder {
        let key = seal::key_bytes(&crate::flash::flash_key(SECRET).unwrap()).unwrap();
        let phone = crate::db::PhoneInfo { id: Some("PHONE-W".into()), name: Some("Worker phone".into()), app: None, ..Default::default() };
        let sealed = seal::seal(&key, &seal::flash_json(&phone, &crate::fakephone::synthetic_items(0, items))).unwrap();
        Encoder::new(session, &sealed, 300).unwrap()
    }

    /// Waits (a long while at most) until the thread says something.
    fn until(worker: &FlashWorker, what: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = worker.status();
            if what(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// While a flash is being put together, the page still hears back: where the flash
    /// stands at once, and for a picture, after a short wait at most, what the picture held.
    /// zone 0.1.0 made both wait for the whole of it.
    #[test]
    fn the_page_hears_back_while_a_flash_is_put_together() {
        let db = library();
        let worker = FlashWorker::start_waiting(db.clone(), Duration::from_millis(200));
        let encoder = flash(60, 0x8000_0f01);
        worker.gate(true);
        let mut index = 0;
        let held = loop {
            let status = worker.frame(encoder.frame(index));
            if status["work"] == "solving" {
                break status;
            }
            index += 1;
            assert!(index < 1000);
        };
        assert_eq!(held["state"], "storing", "{held}");
        // Asked where it stands: answered from what the thread last said, at once.
        let asked = Instant::now();
        let status = worker.status();
        assert!(asked.elapsed() < Duration::from_millis(100));
        assert_eq!((status["state"].as_str(), status["work"].as_str()), (Some("storing"), Some("solving")));
        // A picture's codes wait their turn; the answer comes after the short wait and says
        // the picture held a frame, so the page knows the phone is still in view.
        let posted = Instant::now();
        let answer = worker.picture(vec![encoder.frame(index + 1), b"not a flash".to_vec()], false);
        assert!(posted.elapsed() < Duration::from_secs(5), "{:?}", posted.elapsed());
        assert_eq!((answer["last"]["codes"].as_u64(), answer["last"]["frames"].as_u64()), (Some(2), Some(1)), "{answer}");
        assert_eq!(answer["state"], "storing");
        worker.gate(false);
        let done = until(&worker, |s| s["state"] == "done");
        assert_eq!((done["result"]["new"].as_u64(), done["result"]["paired"].as_bool(), done["work"].clone()), (Some(60), Some(true), Value::Null));
        assert_eq!(db.lock().unwrap().total_items().unwrap(), 60);
        // The picture that waited was taken in after: a frame of a finished flash.
        assert_eq!(until(&worker, |s| s["pictures"]["seen"] == 1)["last"]["frames"], 1);
    }

    /// Starting over stops a flash being put together, and nothing of it is stored.
    #[test]
    fn starting_over_stops_a_flash_being_put_together() {
        let db = library();
        let worker = Arc::new(FlashWorker::start_waiting(db.clone(), Duration::from_millis(200)));
        let encoder = flash(40, 0x8000_0f02);
        worker.gate(true);
        let mut index = 0;
        while worker.frame(encoder.frame(index))["work"] != "solving" {
            index += 1;
        }
        let resetting = std::thread::spawn({
            let worker = worker.clone();
            move || worker.reset()
        });
        // The reset is waiting behind the solve; let the solve go, and it gives up at once.
        until(&worker, |_| worker.inner.cancel.load(Ordering::Relaxed));
        worker.gate(false);
        assert_eq!(resetting.join().unwrap()["state"], "waiting");
        assert_eq!(db.lock().unwrap().total_items().unwrap(), 0);
        // Shown again from the start, it is read and stored as if for the first time.
        for index in 0..200 {
            worker.frame(encoder.frame(index));
        }
        let done = until(&worker, |s| s["state"] == "done");
        assert_eq!(done["result"]["new"], 40, "{done}");
    }

    /// Start over reaches into the one long step of taking a code in, making a flash's rows
    /// again from every reading: the thread stops it at once, the page hears back, and nothing
    /// is stored. (Here the rows are made again because the flash got stuck: code 4 was misread
    /// the same way in two showings, see `Assembler::unstick`.) zone 0.1.1 as first built only
    /// looked between codes, and one code could make the rows again up to eight times, each
    /// costing what the whole flash had so far: about five seconds at 16,384 pieces.
    #[test]
    fn starting_over_stops_making_the_rows_again() {
        let db = library();
        let worker = Arc::new(FlashWorker::start_waiting(db.clone(), Duration::from_millis(200)));
        let encoder = flash(400, 0x8000_0f04);
        let k = encoder.k() as u32;
        assert!(k > 2 * crate::flash::assemble::CONFIRM_APART as u32, "k = {k}");
        let mut bad = encoder.frame(3);
        bad[fountain::HEADER_LEN + 21] ^= 0x5a;
        worker.hold(true);
        // The plain codes twice, code 4 misread both times; then round the loop, read right.
        let passes = (0..2).flat_map(|_| 0..k).map(|index| if index == 3 { bad.clone() } else { encoder.frame(index) });
        let loops = (0..12 * (3 * k + 60)).map(|n| encoder.frame(n % (3 * k + 60)));
        for frame in passes.chain(loops) {
            let status = worker.frame(frame);
            assert_ne!(status["state"], "done", "{status}");
            if worker.is_held() {
                break;
            }
        }
        assert!(worker.is_held(), "the rows were made again");
        let asked = worker.inner.asked.load(Ordering::Relaxed);
        let resetting = std::thread::spawn({
            let worker = worker.clone();
            move || worker.reset()
        });
        until(&worker, |_| worker.inner.cancel.load(Ordering::Relaxed));
        let let_go = Instant::now();
        worker.hold(false);
        assert_eq!(resetting.join().unwrap()["state"], "waiting");
        assert!(let_go.elapsed() < Duration::from_secs(5), "{:?}", let_go.elapsed());
        // Told to stop at the first asking, it asked no more: the rows were left half made.
        assert_eq!(worker.inner.asked.load(Ordering::Relaxed), asked);
        assert_eq!(db.lock().unwrap().total_items().unwrap(), 0);
        // Shown again, read right, it is stored as if for the first time.
        for index in 0..3 * k {
            if worker.frame(encoder.frame(index))["state"] == "done" {
                break;
            }
        }
        let done = until(&worker, |s| s["state"] == "done");
        assert_eq!(done["result"]["new"], 400, "{done}");
    }

    /// A flash stored before zone restarted is known to the next zone at once: its frames,
    /// still on the phone, say so instead of storing it again.
    #[test]
    fn a_restarted_zone_knows_the_flashes_it_stored() {
        let db = library();
        let encoder = flash(25, 0x8000_0f03);
        {
            let worker = FlashWorker::start_waiting(db.clone(), Duration::from_secs(60));
            let mut index = 0;
            while worker.frame(encoder.frame(index))["state"] != "done" {
                index += 1;
            }
        }
        let worker = FlashWorker::start_waiting(db.clone(), Duration::from_secs(60));
        assert_eq!(worker.status()["state"], "waiting");
        let status = worker.frame(encoder.frame(3));
        assert_eq!((status["state"].as_str(), status["result"]["already_stored"].as_bool(), status["result"]["items"].as_u64()), (Some("done"), Some(true), Some(25)), "{status}");
        assert_eq!(worker.reset()["state"], "waiting");
        assert_eq!(worker.frame(encoder.frame(9))["result"]["already_stored"], true);
        let events = db.lock().unwrap().events(None, None, 100).unwrap();
        assert_eq!(events.iter().filter(|e| e.event == "flash_received").count(), 1);
    }
}
