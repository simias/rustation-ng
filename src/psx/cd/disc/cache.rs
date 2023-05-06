//! Multi-threaded prefetching cache for PSX discs.
//!
//! This cache tries to read sectors ahead of the emulator to avoid any I/O lockup

use cdimage::sector::Sector;
use cdimage::DiscPosition;
use cdimage::{Image, Toc};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

pub struct Cache {
    /// The reader state and a Condvar used to notify the reader when it should read a new sector
    /// (or used by the reader to let the cache know that a sector has been read).
    reader: Arc<(Mutex<Reader>, Condvar)>,
    /// Thread handle for the prefetcher
    handle: Option<thread::JoinHandle<()>>,
    /// CD table of contents
    toc: Toc,
}

impl Cache {
    pub fn new(image: Box<dyn Image + Send>) -> Cache {
        let toc = image.toc().clone();

        Cache::new_with_toc(image, toc)
    }

    pub fn new_with_toc(image: Box<dyn Image + Send>, toc: Toc) -> Cache {
        let reader = Arc::new((Mutex::new(Reader::new()), Condvar::new()));

        let thread_reader = reader.clone();

        let builder = thread::Builder::new()
            .name("RSX CD prefetch".to_string())
            .stack_size(1024 * 1024);

        let handle = builder
            .spawn(move || {
                run_prefetcher(image, thread_reader);
            })
            .unwrap();

        Cache {
            reader,
            handle: Some(handle),
            toc,
        }
    }

    fn reader(&self) -> (MutexGuard<Reader>, &Condvar) {
        let (reader, cond) = &*self.reader;

        (reader.lock().unwrap(), cond)
    }

    pub fn read_sector(&mut self, dp: DiscPosition) -> CachedResult<Sector> {
        let (mut reader, cond) = self.reader();

        loop {
            // Even if the sector is already in cache we want to notify the prefetcher of what
            // we're doing so that it can read-ahead if necessary
            reader.prefetch_next = dp;
            reader.prefetch_remaining = PREFETCH_READAHEAD_SECTORS;
            cond.notify_one();

            if let Some(s) = reader.sectors.get(&dp) {
                // Sector is in cache
                return s.clone();
            }

            // Sector isn't cached, wait for the prefetcher
            reader = cond.wait(reader).unwrap();
        }
    }

    pub fn toc(&self) -> &Toc {
        &self.toc
    }
}

impl ::std::ops::Drop for Cache {
    fn drop(&mut self) {
        {
            let (mut reader, cond) = self.reader();

            // Tell the prefetcher to quit
            reader.quit = true;
            cond.notify_one();
        }

        if let Some(t) = self.handle.take() {
            t.join().unwrap();
        }
    }
}

/// We need to store the error in an Arc because it can't be cloned
pub type CachedResult<R> = std::result::Result<R, Arc<cdimage::CdError>>;

type SectorCache = fnv::FnvHashMap<DiscPosition, CachedResult<Sector>>;

/// The shared state between the main thread and the prefetcher
struct Reader {
    /// The actual sector cache
    sectors: SectorCache,
    /// Number of sectors left to prefetch before becoming idle
    prefetch_remaining: u32,
    /// Next sector we should attempt to prefetch (if `prefetch_remaining` is > 0).
    prefetch_next: DiscPosition,
    /// Set to true when the prefetcher should quit
    quit: bool,
}

impl Reader {
    fn new() -> Reader {
        Reader {
            sectors: SectorCache::with_capacity_and_hasher(CACHE_CAPACITY, Default::default()),
            prefetch_remaining: 0,
            prefetch_next: DiscPosition::INNERMOST,
            quit: false,
        }
    }
}

fn run_prefetcher(mut image: Box<dyn Image>, reader: Arc<(Mutex<Reader>, Condvar)>) {
    let (reader_mutex, cond) = &*reader;

    let mut reader = reader_mutex.lock().unwrap();

    while !reader.quit {
        if reader.prefetch_remaining == 0 {
            // Nothing left to do, wait for the next prefetch command
            reader = cond.wait(reader).unwrap();
            continue;
        }

        // We have something to prefetch
        let fetch_msf = reader.prefetch_next;

        // Update prefetch_remaining and prefetch_next before we drop the lock since the emulator
        // code can override those.
        match reader.prefetch_next.next() {
            Some(next) => {
                reader.prefetch_remaining -= 1;
                reader.prefetch_next = next;
            }
            None => {
                // Reached max MSF, nothing left to do
                reader.prefetch_remaining = 0;
            }
        }

        if reader.sectors.contains_key(&fetch_msf) {
            // We already have this sector
            continue;
        }

        // We drop the lock while the read is taking place to avoid stalling the emulator if it
        // tries to access an already-cached sector
        ::std::mem::drop(reader);

        let sector = match image.read_sector(fetch_msf) {
            Ok(s) => Ok(s),
            Err(e) => Err(Arc::new(e)),
        };

        // Re-lock the reader
        reader = reader_mutex.lock().unwrap();

        reader.sectors.insert(fetch_msf, sector);
        // If the emulator was waiting for a sector, wake it up
        cond.notify_one();
    }
}

/// Number of sectors to read ahead
const PREFETCH_READAHEAD_SECTORS: u32 = 75;

/// Initial capacity of the cache. We'll be able to put that many elements before reallocating.
/// For now we just allow caching an entire 74mn disc. Probably overkill bur RAM it cheap.
const CACHE_CAPACITY: usize = 74 * 60 * 75;
