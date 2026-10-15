//! One announcement burst at a time. Refresh replaces the burst in progress.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use etherquic::discover::{self, Found};

pub struct Board {
    pub looking: bool,
    pub error: Option<String>,
    pub devices: Vec<Found>,
    /// Counts finished bursts, so the window can tell a new answer from the same one.
    pub burst: u64,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            looking: false,
            error: None,
            devices: Vec::new(),
            burst: 0,
        }
    }
}

struct Job {
    nic: Option<String>,
    generation: u64,
}

pub struct Scan {
    tx: Sender<Job>,
    generation: Arc<AtomicU64>,
    board: Arc<Mutex<Board>>,
}

impl Scan {
    pub fn start(nic: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let board = Arc::new(Mutex::new(Board::default()));
        let gen = Arc::clone(&generation);
        let shared = Arc::clone(&board);
        thread::Builder::new()
            .name("etherquic-discover".into())
            .spawn(move || worker(rx, gen, shared))
            .expect("discovery thread");
        let scan = Self {
            tx,
            generation,
            board,
        };
        scan.refresh(nic);
        scan
    }

    pub fn refresh(&self, nic: Option<String>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut board) = self.board.lock() {
            board.looking = true;
            board.error = None;
        }
        let _ = self.tx.send(Job { nic, generation });
    }

    pub fn board(&self) -> Board {
        match self.board.lock() {
            Ok(board) => Board {
                looking: board.looking,
                error: board.error.clone(),
                devices: board.devices.clone(),
                burst: board.burst,
            },
            Err(poisoned) => {
                let board = poisoned.into_inner();
                Board {
                    looking: board.looking,
                    error: board.error.clone(),
                    devices: board.devices.clone(),
                    burst: board.burst,
                }
            }
        }
    }
}

fn worker(rx: mpsc::Receiver<Job>, generation: Arc<AtomicU64>, board: Arc<Mutex<Board>>) {
    while let Ok(mut job) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        let result = discover::discover(job.nic.as_deref(), &generation, job.generation);
        if generation.load(Ordering::SeqCst) != job.generation {
            continue;
        }
        let mut slot = match board.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.looking = false;
        match result {
            Ok(devices) => {
                slot.devices = devices;
                slot.error = None;
                slot.burst += 1;
            }
            Err(err) => slot.error = Some(err),
        }
    }
}
