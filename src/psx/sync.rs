use super::{dma, gpu, mdec, pad_memcard, spu, timers, CycleCount, Psx};

/// Tokens used to keep track of the progress of each module individually
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq, Debug)]
pub enum SyncToken {
    Gpu,
    Timers,
    Spu,
    Dma,
    PadMemCard,
    MDec,

    NumTokens,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Synchronizer {
    /// Array containing, for each module, the date corresponding to the last sync.
    last_sync: [CycleCount; SyncToken::NumTokens as usize],
    /// Array containing, for each module, the date at which we should force a resync.
    next_event: [CycleCount; SyncToken::NumTokens as usize],
    /// The date of the event in `next_event` that occurs first
    first_event: CycleCount,
}

impl Synchronizer {
    pub fn new() -> Synchronizer {
        Synchronizer {
            last_sync: [0; SyncToken::NumTokens as usize],
            next_event: [0; SyncToken::NumTokens as usize],
            first_event: 0,
        }
    }

    pub fn refresh_first_event(&mut self) {
        // The only way `min()` can return None is if the array is empty which is impossible here.
        self.first_event = *self.next_event.iter().min().unwrap();
    }

    pub fn first_event(&self) -> CycleCount {
        self.first_event
    }
}

/// Resynchronize `who` with the CPU, returning the number of CPU cycles elapsed since the last
/// sync date
pub fn resync(psx: &mut Psx, who: SyncToken) -> CycleCount {
    let who = who as usize;

    let elapsed = psx.cycle_counter - psx.sync.last_sync[who];

    if elapsed <= 0 {
        // Since we move the timestamp back when we handle an event it's possible in some cases to
        // end up with an event being handled after a refresh that already put us past it.
        debug_assert!(elapsed > -300);
        return 0;
    }

    psx.sync.last_sync[who] = psx.cycle_counter;

    elapsed
}

/// Reset the cycle_counter to 0 by rebasing all the event counters relative to it. This way we
/// don't have to worry about overflows.
pub fn rebase_counters(psx: &mut Psx) {
    let cc = psx.cycle_counter;

    for i in 0..(SyncToken::NumTokens as usize) {
        psx.sync.last_sync[i] -= cc;
        psx.sync.next_event[i] -= cc;
    }
    psx.sync.first_event -= cc;

    psx.cpu.rebase_counters(cc);

    psx.cycle_counter = 0;
}

/// If `who` couldn't consume all the cycles returned by `resync` it can return the leftover here,
/// we'll move the `last_sync` back by the same number of cycles which means that they'll be
/// returned on the next call to `resync`. Should only be called with *positive* cycle amounts,
/// otherwise it would put the module in the future.
pub fn rewind(psx: &mut Psx, who: SyncToken, rewind: CycleCount) {
    debug_assert!(rewind >= 0);

    psx.sync.last_sync[who as usize] -= rewind;
}

/// Returns true if an event is pending and should be treated
pub fn is_event_pending(psx: &Psx) -> bool {
    psx.cycle_counter >= psx.sync.first_event
}

/// Run event handlers as necessary
pub fn handle_events(psx: &mut Psx) {
    while is_event_pending(psx) {
        // If we've "overshot" the event date (which will almost always happen since CPU
        // instructions usually take more than one cycle to execute) we temporarily rewind to the
        // event date before running the various handlers. One situation where that matters is for
        // timers synchronized on GPU timings: if we overshoot the HSync by a few cycles then we'll
        // freeze the timer too late for instance.
        let event_delta = psx.cycle_counter - psx.sync.first_event;
        psx.cycle_counter -= event_delta;

        if psx.sync.first_event >= psx.sync.next_event[SyncToken::Gpu as usize] {
            gpu::run(psx);
        }

        if psx.sync.first_event >= psx.sync.next_event[SyncToken::Timers as usize] {
            timers::run(psx);
        }

        if psx.sync.first_event >= psx.sync.next_event[SyncToken::Dma as usize] {
            dma::run(psx);
        }

        // SPU sync must come after CDROM since we could be playing back CD audio and we don't want
        // to starve
        if psx.sync.first_event >= psx.sync.next_event[SyncToken::Spu as usize] {
            spu::run(psx);
        }

        if psx.sync.first_event >= psx.sync.next_event[SyncToken::PadMemCard as usize] {
            pad_memcard::run(psx);
        }

        if psx.sync.first_event >= psx.sync.next_event[SyncToken::MDec as usize] {
            mdec::run(psx);
        }

        psx.cycle_counter += event_delta;
    }
}

/// Set the next sync for `who` at `delay` cycles from now
pub fn next_event(psx: &mut Psx, who: SyncToken, delay: CycleCount) {
    psx.sync.next_event[who as usize] = psx.cycle_counter + delay;

    psx.sync.refresh_first_event();
}
