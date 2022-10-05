//! The PlayStation has three timers. They're mostly identical except that they can each select a
//! different clock source besides the regular system clock:
//!
//! - Timer 0: GPU pixel clock
//! - Timer 1: GPU horizontal blanking
//! - Timer 2: System clock / 8

use super::{irq, sync, Addressable, CycleCount, Psx};
use std::cmp::min;
use std::ops::{Index, IndexMut};

const TIMERSYNC: sync::SyncToken = sync::SyncToken::Timers;
const TIMER_IRQ: [irq::Interrupt; 3] = [
    irq::Interrupt::Timer0,
    irq::Interrupt::Timer1,
    irq::Interrupt::Timer2,
];

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Timers {
    timers: [Timer; 3],
    /// True if the GPU is currently in an HSync
    in_hsync: bool,
    /// True if the GPU is currently in a VSync
    in_vsync: bool,
    /// For Timer2: divider for SysClockDiv8
    divider_8: CycleCount,
}

impl Timers {
    pub fn new() -> Timers {
        Timers {
            timers: [Timer::new(), Timer::new(), Timer::new()],
            in_hsync: false,
            in_vsync: false,
            divider_8: 0,
        }
    }

    /// Retrieve the clock source for timer `which`
    fn clock_source(&self, which: usize) -> Clock {
        let source_raw = self[which].mode.clock_source();

        CLOCK_SOURCE_MATRIX[which][source_raw]
    }

    /// Retrieve the sync mode for timer `which`
    fn sync_mode(&self, which: usize) -> SyncMode {
        let timer = &self[which];

        if timer.mode.sync_enabled() {
            let sync_raw = timer.mode.sync_mode();

            SYNC_MODE_MATRIX[which][sync_raw]
        } else {
            SyncMode::FreeRun
        }
    }

    fn next_irq(&self) -> Option<CycleCount> {
        let mut delta = None;

        for which in 0..3 {
            let source = self.clock_source(which);
            let sync_mode = self.sync_mode(which);

            if let Some(mut d) = self[which].next_irq(source, sync_mode) {
                if source == Clock::CpuDiv8 {
                    d *= 8;
                    d -= self.divider_8;
                }

                let new_d = match delta {
                    None => d,
                    Some(delta) => min(d, delta),
                };

                delta = Some(new_d);
            }
        }

        delta
    }

    /// Run the timer `which` for `cpu_cycles`. Returns true if an interrupt was triggered.
    fn run_cpu(&mut self, which: usize, cpu_cycles: CycleCount) -> bool {
        let source = self.clock_source(which);

        if which == 2 {
            // Run the /8 divider
            // XXX I think this runs all the time, even when an other source is selected but I
            // haven't double-checked it.
            self.divider_8 += cpu_cycles;
            let ticks = self.divider_8 / 8;
            self.divider_8 &= 7;

            if source == Clock::CpuDiv8 {
                return self[which].run(ticks);
            }
        }

        self[which].run_cpu(source, cpu_cycles)
    }

    /// Run the timer `which` for `gpu_cycles`. Returns true if an interrupt was triggered.
    fn run_gpu_clock(&mut self, which: usize, gpu_cycles: CycleCount) -> bool {
        let source = self.clock_source(which);

        self[which].run_gpu_clock(source, gpu_cycles)
    }

    /// Run the timer `which` for `hsync_cycles`. Returns true if an interrupt was triggered.
    fn run_gpu_hsync(&mut self, which: usize, hsync_cycles: CycleCount) -> bool {
        let source = self.clock_source(which);

        self[which].run_gpu_hsync(source, hsync_cycles)
    }

    fn set_in_hsync(&mut self, entered_hsync: bool) {
        self.in_hsync = entered_hsync;
        self.refresh_sync();

        // Counter 0 can start on HSync
        let t0 = &mut self[0];

        if entered_hsync {
            if t0.sync_state == SyncState::WaitingForSyncStart {
                // We'll start at the end of HSync
                t0.sync_state = SyncState::WaitingForSyncEnd;
            }
        } else if t0.sync_state == SyncState::WaitingForSyncEnd {
            // We're running
            t0.sync_state = SyncState::Running;
        }
    }

    fn set_in_vsync(&mut self, entered_vsync: bool) {
        self.in_vsync = entered_vsync;
        self.refresh_sync();

        // Counter 1 can start on VSync
        let t1 = &mut self[1];

        if entered_vsync {
            if t1.sync_state == SyncState::WaitingForSyncStart {
                // We'll start at the end of HSync
                t1.sync_state = SyncState::WaitingForSyncEnd;
            }
        } else if t1.sync_state == SyncState::WaitingForSyncEnd {
            // We're running
            t1.sync_state = SyncState::Running;
        }
    }

    /// Check if timer synchronization config and start or stop them as needed
    fn refresh_sync(&mut self) {
        for which in 0..3 {
            let sync_mode = self.sync_mode(which);
            let in_hsync = self.in_hsync;
            let in_vsync = self.in_vsync;

            self[which].refresh_sync(sync_mode, in_hsync, in_vsync);
        }
    }
}

impl Index<usize> for Timers {
    type Output = Timer;

    fn index(&self, index: usize) -> &Self::Output {
        &self.timers[index]
    }
}

impl IndexMut<usize> for Timers {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.timers[index]
    }
}

/// Timer synchronization state
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
enum SyncState {
    /// Timer is stopped
    Stopped,
    /// Timer runs normally
    Running,
    /// Timer waits for for (H|V)Sync start
    WaitingForSyncStart,
    /// Timer waits for for (H|V)Sync end
    WaitingForSyncEnd,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Timer {
    mode: Mode,
    /// The counter is really 16bit but we use a wider value to avoid overflows (since we might
    /// overshoot by a few cycles before we handle the overflow in `run`). It also makes the code
    /// dealing with `set_overflow` and `set_target` simpler.
    counter: CycleCount,
    target: u16,
    /// If the IRQ is configured to be oneshot in `Mode` we don't re-trigger.
    irq_inhibit: bool,
    /// Timer state machine for synchronization
    sync_state: SyncState,
}

impl Timer {
    fn new() -> Timer {
        Timer {
            mode: Mode::new(),
            counter: 0,
            target: 0,
            irq_inhibit: false,
            sync_state: SyncState::Running,
        }
    }

    fn counter(&self) -> u16 {
        debug_assert!(self.counter >= 0 && self.counter <= 0xffff);
        self.counter as u16
    }

    fn write_counter(&mut self, val: u16) {
        self.counter = CycleCount::from(val);
        self.irq_inhibit = false;
    }

    fn set_mode(&mut self, mode: u16) {
        self.mode.configure(mode);

        // Writing to the mode register resets the counter to 0 and re-enables the IRQ if
        // necessary.
        self.write_counter(0);

        // Assume that the counter is stopped for a start, `refresh_sync` will take care of fixing
        // that if it's not appropriate. It's important to set it to Stopped and not Running in
        // case the timer is supposed to start on (H|V)Sync.
        self.sync_state = SyncState::Stopped;
    }

    fn read_mode(&mut self) -> u16 {
        let mode = self.mode.0;

        // Some bits are read-clear
        self.mode.clear_overflow_reached();
        if !self.target_match() {
            self.mode.clear_target_reached();
        }

        mode
    }

    fn set_target(&mut self, val: u16) {
        self.target = val;
    }

    /// Returns true if `counter` equals `target`
    fn target_match(&self) -> bool {
        CycleCount::from(self.target) == self.counter
    }

    /// Called when a target mach occurred. Returns `true` if the interrupt has been triggered.
    fn set_match(&mut self) -> bool {
        self.mode.set_target_reached();

        if self.mode.reset_counter_on_target() {
            if self.target == 0 {
                self.counter = 0;
            } else {
                self.counter %= CycleCount::from(self.target);
            }
        }

        if self.mode.irq_on_target() && !self.irq_inhibit {
            if self.mode.one_shot_irq() {
                self.irq_inhibit = true;
            }
            true
        } else {
            false
        }
    }

    /// Called when a counter overflow occurred. Returns `true` if the interrupt has been
    /// triggered.
    fn set_overflow(&mut self) -> bool {
        self.mode.set_overflow_reached();

        self.counter &= 0xffff;

        if self.mode.irq_on_overflow() && !self.irq_inhibit {
            if self.mode.one_shot_irq() {
                self.irq_inhibit = true;
            }
            true
        } else {
            false
        }
    }

    /// Predict the date of the next interrupt (or `None` if the interrupt is disabled or can't be
    /// predicted)
    fn next_irq(&self, source: Clock, sync_mode: SyncMode) -> Option<CycleCount> {
        if !self.mode.irq_on_target() && !self.mode.irq_on_overflow() {
            // No interrupt is configured
            return None;
        }

        if self.irq_inhibit {
            // IRQ won't trigger
            return None;
        }

        if source == Clock::GpuHSync {
            // We're going to be synchronized by the GPU code, we have nothing to do
            return None;
        }

        if source == Clock::GpuPixClk {
            // XXX This one will be imprecise because the GPU pixel clock counter is (obviously)
            // not refreshed at every tick. We could actually predict when it overflows using the
            // GPU clock ratio and some clever maths but mednafen doesn't bother with it so let's
            // ignore it for now
            return None;
        }

        if sync_mode == SyncMode::Stopped {
            // Timer is stopped. XXX Might be worth checking what happens if we're stuck on target
            // and irq_on_target is true.
            return None;
        }

        let target = CycleCount::from(self.target);

        // Value of the counter the on the next event
        let event_counter = if self.counter > target {
            // We've overshot the target, the next event is on overflow. Technically if the
            // overflow IRQ is disabled we could micro-optimize this to only force a refresh
            // when the counter will have wrapped all the way back to the target but it
            // probably isn't worth it.
            0x1_0000
        } else {
            // We haven't reached the target yet
            if self.mode.irq_on_target() || self.mode.reset_counter_on_target() {
                // We need to force a refresh when we hit the target to either trigger the IRQ
                // or reset the counter. XXX actually I'm fairly sure that we don't need to force a
                // sync when we `reset_counter_on_target` as long as there's no `irq_on_target`.
                target
            } else {
                // We don't have anything special to do when we pass the target, we can aim
                // directly for the overflow
                0x1_0000
            }
        };

        let mut delta = event_counter - self.counter;

        if delta == 0 {
            // This can happen if target == counter == 0 and we reset_counter_on_target. Seems like
            // a terrible because that'll trigger the interrupt continuously (XXX I think? Need to
            // double-check).
            warn!("Timer sync delta is 0");
            delta = 1;
        }

        Some(delta as CycleCount)
    }

    /// Advance the counter by `cycles`. Returns true if an interrupt has been triggered
    fn run(&mut self, cycles: CycleCount) -> bool {
        if self.mode.reset_counter_on_target() && self.target == 0 && self.target_match() {
            // This is a weird situation, we need to reset the counter to 0 when we reach the
            // target, the target is 0, and we've reached it.
            return self.set_match();
        }

        if cycles == 0 || self.sync_state != SyncState::Running {
            // XXX Shouldn't we trigger the IRQ anyway here if we're still at the match? Does the
            // IRQ keep triggering if SyncMode is Stopped and counter == target for instance? In
            // this case we might want to avoid running this code if `cycles` is 0 in other
            // circumstances.
            return false;
        }

        let before_counter = self.counter;

        self.counter += cycles;

        let target = CycleCount::from(self.target);

        let target_irq = if before_counter < target && self.counter >= target {
            // We reached the target
            self.set_match()
        } else {
            false
        };

        // If the target was reached and `reset_counter_on_target` is true then we simply cannot
        // overflow but that's handled correctly here because `self.set_match` is called above if
        // we've hit the target and it'll already have readjusted `self.counter` if
        // `reset_counter_on_target` is active. For this reason we must always check for overflow
        // *after* we check for target match.
        let overflow_irq = if self.counter > 0xffff {
            self.set_overflow()
        } else {
            false
        };

        target_irq || overflow_irq
    }

    /// Run the timer for `cpu_cycles` cycles. Returns true if an interrupt has been triggered.
    fn run_cpu(&mut self, source: Clock, cpu_cycles: CycleCount) -> bool {
        let mut cycles = cpu_cycles;

        if source == Clock::CpuDiv8 {
            // Should be handled by Timer::run_cpu directly
            unreachable!();
        }

        if source == Clock::GpuPixClk || source == Clock::GpuHSync {
            // We're clocked from the GPU, we have nothing to do here
            cycles = 0;
        }

        // Now we can use switch to the generic code, shared between CPU and GPU
        self.run(cycles)
    }

    /// Run the timer for `gpu_cycles` cycles. Returns true if an interrupt has been triggered.
    fn run_gpu_clock(&mut self, source: Clock, gpu_cycles: CycleCount) -> bool {
        if source == Clock::GpuPixClk {
            self.run(gpu_cycles)
        } else {
            // We're not clocked from the GPU, nothing to do.
            false
        }
    }

    /// Run the timer for `hsync_cycles` cycles. Returns true if an interrupt has been triggered.
    fn run_gpu_hsync(&mut self, source: Clock, hsync_cycles: CycleCount) -> bool {
        if source == Clock::GpuHSync {
            self.run(hsync_cycles)
        } else {
            // We're not clocked from the GPU, nothing to do.
            false
        }
    }

    fn refresh_sync(&mut self, sync_mode: SyncMode, in_hsync: bool, in_vsync: bool) {
        self.sync_state = match sync_mode {
            SyncMode::FreeRun => SyncState::Running,
            SyncMode::Stopped => SyncState::Stopped,
            SyncMode::StartOnNextLine | SyncMode::StartOnNextFrame => {
                if self.sync_state == SyncState::Stopped {
                    SyncState::WaitingForSyncStart
                } else {
                    // Either we're already running and we have nothing to do, or we're waiting for
                    // the sync and that's handled in set_in_vsync/set_in_hsync. We have nothing to
                    // do here.
                    self.sync_state
                }
            }
            SyncMode::ResetCounterOnHsync => SyncState::Running,
            SyncMode::ResetCounterOnVsync => SyncState::Running,
            SyncMode::HSyncOnly => {
                if in_hsync {
                    SyncState::Running
                } else {
                    SyncState::Stopped
                }
            }
            SyncMode::VSyncOnly => {
                if in_vsync {
                    SyncState::Running
                } else {
                    SyncState::Stopped
                }
            }
            SyncMode::PauseOnHsync => {
                if in_hsync {
                    SyncState::Stopped
                } else {
                    SyncState::Running
                }
            }
            SyncMode::PauseOnVsync => {
                if in_vsync {
                    SyncState::Stopped
                } else {
                    SyncState::Running
                }
            }
        };
    }
}

/// Run all three timers, triggering interrupts if needed
fn run_timers(psx: &mut Psx) {
    let elapsed = sync::resync(psx, TIMERSYNC);

    for (which, &irq) in TIMER_IRQ.iter().enumerate() {
        if psx.timers.run_cpu(which, elapsed) {
            irq::trigger(psx, irq);
        }
    }
}

pub fn run(psx: &mut Psx) {
    run_timers(psx);
    predict_next_sync(psx);
}

/// Called from the GPU to advance the counters working on the GPU Pixel Clock
pub fn run_gpu_clocks(psx: &mut Psx, gpu_clock_cycles: CycleCount) {
    // Only Timer 0 can work on the GPU clock
    let which = 0;

    if psx.timers.run_gpu_clock(which, gpu_clock_cycles) {
        irq::trigger(psx, TIMER_IRQ[which]);
    }

    // Since we don't currently predict GPU clock sync events there's no need to call
    // predict_next_sync here.
}

/// Called from the GPU when we enter or leave the HSync
pub fn set_in_hsync(psx: &mut Psx, entered_hsync: bool) {
    run_timers(psx);

    psx.timers.set_in_hsync(entered_hsync);

    // Timer 0's counter can optionally be reset to 0 when we enter *or* leave HSync
    let which = 0;

    let sync_mode = psx.timers.sync_mode(which);

    let needs_reset = if entered_hsync {
        sync_mode == SyncMode::ResetCounterOnHsync
    } else {
        // XXX No$ says that the counter is reset to 0 when we enter HBlank, mednafen does it when
        // we leave it
        sync_mode == SyncMode::HSyncOnly
    };

    if needs_reset {
        // Don't call `write_counter` because it shouldn't re-enable the IRQ if it's inhibited
        psx.timers[which].counter = 0;

        if psx.timers[which].target_match() && psx.timers[which].set_match() {
            irq::trigger(psx, TIMER_IRQ[which]);
        }
    }

    if entered_hsync {
        // Timer 1 can optionally count the number of HSyncs
        let which = 1;

        if psx.timers.run_gpu_hsync(which, 1) {
            irq::trigger(psx, TIMER_IRQ[which]);
        }
    }

    predict_next_sync(psx);
}

/// Called from the GPU when we enter or leave the VSync
pub fn set_in_vsync(psx: &mut Psx, entered_vsync: bool) {
    run_timers(psx);

    psx.timers.set_in_vsync(entered_vsync);

    // Timer 1's counter can optionally be reset to 0 when we enter *or* leave VSync
    let which = 1;

    let sync_mode = psx.timers.sync_mode(which);

    let needs_reset = if entered_vsync {
        sync_mode == SyncMode::ResetCounterOnVsync
    } else {
        // XXX No$ says that the counter is reset to 0 when we enter VBlank, mednafen does it when
        // we leave it
        sync_mode == SyncMode::VSyncOnly
    };

    if needs_reset {
        // Don't call `write_counter` because it shouldn't re-enable the IRQ if it's inhibited
        psx.timers[which].counter = 0;

        if psx.timers[which].target_match() && psx.timers[which].set_match() {
            irq::trigger(psx, TIMER_IRQ[which]);
        }
    }

    predict_next_sync(psx);
}

/// Figure out when we should force a resync next
fn predict_next_sync(psx: &mut Psx) {
    // Default value used if we don't have any upcoming event to monitor for. Mednafen uses 1024
    // but I'm not sure why it needs to be so short when to event is scheduled. Technically we
    // could wait for a long time, as long as we don't overflow the CycleCount.
    let default = 0x1_0000;

    let delta = psx.timers.next_irq().unwrap_or(default);

    sync::next_event(psx, TIMERSYNC, delta);
}

pub fn load<T: Addressable>(psx: &mut Psx, offset: u32) -> T {
    run(psx);

    let which = (offset >> 4) as usize;

    let timer = &mut psx.timers[which];

    let v = match offset & 0xf {
        0x0 => timer.counter(),
        0x4 => timer.read_mode(),
        0x8 => timer.target,
        n => unimplemented!("timer read @ {:x}", n),
    };

    T::from_u32(u32::from(v))
}

pub fn store<T: Addressable>(psx: &mut Psx, offset: u32, val: T) {
    run(psx);

    let val = val.as_u16();
    let which = (offset >> 4) as usize;

    match offset & 0xf {
        0x0 => psx.timers[which].write_counter(val),
        0x4 => {
            psx.timers[which].set_mode(val);
            psx.timers.refresh_sync();
        }
        0x8 => psx.timers[which].set_target(val),
        0xc => (), // Nothing in this register
        n => unimplemented!("timer write @ {:x}", n),
    }

    // Check if a match happened as a consequence of the register writes
    if psx.timers[which].target_match() && psx.timers[which].set_match() {
        irq::trigger(psx, TIMER_IRQ[which]);
    }

    predict_next_sync(psx);
}

/// Timer mode register
#[derive(serde::Serialize, serde::Deserialize)]
struct Mode(u16);

impl Mode {
    fn new() -> Mode {
        Mode(0)
    }

    fn configure(&mut self, mode: u16) {
        // bits [10:12] are read-only and must be preserved
        // XXX We don't implement bit 10 (Interrupt Request) for the time being.
        self.0 &= 7 << 10;
        self.0 |= mode & 0x3ff;
    }

    fn set_target_reached(&mut self) {
        self.0 |= 1 << 11;
    }

    fn clear_target_reached(&mut self) {
        self.0 &= !(1 << 11);
    }

    fn set_overflow_reached(&mut self) {
        self.0 |= 1 << 12;
    }

    fn clear_overflow_reached(&mut self) {
        self.0 &= !(1 << 12);
    }

    /// Returns true if this counter should trigger an interrupt when the target is reached
    fn irq_on_target(&self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// Returns true if this counter should trigger an interrupt when the counter overflows
    fn irq_on_overflow(&self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// Returns true if the counter should be reset to 0 when the target is reached, false if it
    /// should just wrap around when it reaches 0xffff.
    fn reset_counter_on_target(&self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// If true the IRQ is one-shot: it will only trigger once and then remain inactive until
    /// rearmed by a register write to the timer mode or counter.
    fn one_shot_irq(&self) -> bool {
        self.0 & (1 << 6) == 0
    }

    /// Get the clock source configuration. The actual source will depend on which timer is
    /// configured.
    fn clock_source(&self) -> usize {
        let source = (self.0 >> 8) & 3;

        source as usize
    }

    /// Returns true if synchronization is enabled
    fn sync_enabled(&self) -> bool {
        self.0 & 1 != 0
    }

    /// Get the sync mode configuration. The actual mode will depend on which timer is
    /// configured.
    fn sync_mode(&self) -> usize {
        let mode = (self.0 >> 1) & 3;

        mode as usize
    }
}

/// The possible source of synchronization for the timers
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum SyncMode {
    /// Counter is free-running
    FreeRun,
    /// Counter is stopped
    Stopped,
    /// Counter is reset to 0 when we enter HSync
    ResetCounterOnHsync,
    /// Count only inside HSync and reset to 0 when we leave it
    HSyncOnly,
    /// Couster is paused on HSync
    PauseOnHsync,
    /// Counter is reset to 0 when we enter VSync
    ResetCounterOnVsync,
    /// Count only inside VSync and reset to 0 when we leave it
    VSyncOnly,
    /// Couster is paused on VSync
    PauseOnVsync,
    /// Counter starts counting after a full HSync has passed
    StartOnNextLine,
    /// Counter starts counting after a full VSync has passed
    StartOnNextFrame,
}

/// Look up table to get the actual sync mode from the Mode config for each of the 3 timer
const SYNC_MODE_MATRIX: [[SyncMode; 4]; 3] = [
    [
        SyncMode::PauseOnHsync,
        SyncMode::ResetCounterOnHsync,
        SyncMode::HSyncOnly,
        SyncMode::StartOnNextLine,
    ],
    [
        SyncMode::PauseOnVsync,
        SyncMode::ResetCounterOnVsync,
        SyncMode::VSyncOnly,
        SyncMode::StartOnNextFrame,
    ],
    [
        SyncMode::Stopped,
        SyncMode::FreeRun,
        SyncMode::Stopped,
        SyncMode::FreeRun,
    ],
];

/// The four possible clock sources for the timers. Not every timer can use every clock.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Clock {
    /// The CPU clock at ~33.87MHz
    Cpu,
    /// The CPU clock divided by 8 (~4.23MHz)
    CpuDiv8,
    /// The GPU's pixelclock (depends on hardware, around 53Mhz)
    GpuPixClk,
    /// The GPU's HSync signal (deponds on hardware and video timings)
    GpuHSync,
}

/// Look up table to get the actual clock source from the Mode config for each of the 3 timer
const CLOCK_SOURCE_MATRIX: [[Clock; 4]; 3] = [
    [Clock::Cpu, Clock::GpuPixClk, Clock::Cpu, Clock::GpuPixClk],
    [Clock::Cpu, Clock::GpuHSync, Clock::Cpu, Clock::GpuHSync],
    [Clock::Cpu, Clock::Cpu, Clock::CpuDiv8, Clock::CpuDiv8],
];
