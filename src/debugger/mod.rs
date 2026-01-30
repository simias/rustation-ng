use std::net::TcpListener;

use crate::psx::Psx;
use crate::psx::debugger::Debugger as DebuggerInterface;
use crate::psx::map::mask_region;

use self::gdb::GdbRemote;

mod bios;
mod gdb;

/// Rustation-libretro debugger, based on the GDB remote serial interface
pub struct Debugger {
    /// Listener waiting for remote connections
    listener: TcpListener,
    /// Holds the current client connection
    client: Option<GdbRemote>,
    /// Internal state: set to true when the remote requests that the execution should resume
    resume: bool,
    /// If a single step is requested this flag is set
    step: bool,
    /// Vector containing all active breakpoint addresses
    breakpoints: Vec<u32>,
    /// Vector containing all active read watchpoints
    read_watchpoints: Vec<u32>,
    /// Vector containing all active write watchpoints
    write_watchpoints: Vec<u32>,
    /// If true we additionally log BIOS calls
    log_bios_calls: bool,
}

impl Debugger {
    pub fn new() -> Debugger {
        let bind_to = "127.0.0.1:9001";

        // XXX The bind address/port should be configurable
        let listener = match TcpListener::bind(bind_to) {
            Ok(l) => l,
            Err(e) => panic!("Couldn't bind GDB server TCP socket: {}", e),
        };

        info!("Waiting for debugger on {}", bind_to);

        Debugger {
            listener,
            client: None,
            resume: true,
            step: false,
            breakpoints: Vec::new(),
            read_watchpoints: Vec::new(),
            write_watchpoints: Vec::new(),
            log_bios_calls: false,
        }
    }

    pub fn set_log_bios_calls(&mut self, enable: bool) {
        self.log_bios_calls = enable;
    }

    fn debug(&mut self, psx: &mut Psx) {
        // If stepping was requested we can reset the flag here, this way we won't "double step" if
        // we're entering debug mode for an other reason (data watchpoint for instance)
        self.step = false;

        let mut client = match self.client.take() {
            Some(mut c) => {
                // Notify the remote that we're halted and waiting for instructions. I ignore
                // errors here for simplicity, if the connection hung up for some reason we'll
                // figure it out soon enough.
                let _ = c.send_status();
                c
            }
            None => GdbRemote::new(&self.listener),
        };

        // We loop as long as the remote debugger doesn't tell us to continue
        self.resume = false;

        while !self.resume {
            // Inner debugger loop: handle client requests until it requests that the execution
            // resumes or an error is encountered
            if client.serve(self, psx).is_err() {
                // We encountered an error with the remote client: we wait for a new connection
                client = GdbRemote::new(&self.listener);
            }
        }

        // Before we resume execution we store the current client
        self.client = Some(client);
    }

    fn resume(&mut self) {
        self.resume = true;
    }

    fn set_step(&mut self) {
        self.step = true;
    }

    /// Add a breakpoint that will trigger when the instruction at `addr` is about to be executed.
    fn add_breakpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        // Make sure we're not adding the same address twice
        if !self.breakpoints.contains(&addr) {
            self.breakpoints.push(addr);
        }
    }

    /// Delete breakpoint at `addr`. Does nothing if there was no breakpoint set for this address.
    fn del_breakpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        self.breakpoints.retain(|&a| a != addr);
    }

    /// Add a breakpoint that will trigger when the psx attempts to read from `addr`
    fn add_read_watchpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        // Make sure we're not adding the same address twice
        if !self.read_watchpoints.contains(&addr) {
            self.read_watchpoints.push(addr);
        }
    }

    /// Delete read watchpoint at `addr`. Does nothing if there was no breakpoint set for this
    /// address.
    fn del_read_watchpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        self.read_watchpoints.retain(|&a| a != addr);
    }

    /// Add a breakpoint that will trigger when the psx attempts to write to `addr`
    fn add_write_watchpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        // Make sure we're not adding the same address twice
        if !self.write_watchpoints.contains(&addr) {
            self.write_watchpoints.push(addr);
        }
    }

    /// Delete write watchpoint at `addr`. Does nothing if there was no breakpoint set for this
    /// address.
    fn del_write_watchpoint(&mut self, addr: u32) {
        let addr = mask_region(addr);

        self.write_watchpoints.retain(|&a| a != addr);
    }
}

impl DebuggerInterface for Debugger {
    /// Signal a "break" which will trigger the debugger
    fn trigger_break(&mut self, psx: &mut Psx) {
        self.debug(psx);
    }

    /// Called by the psx when it's about to execute a new instruction. This function is called
    /// before *all* PSX instructions so it needs to be as fast as possible.
    fn pc_change(&mut self, psx: &mut Psx) {
        let pc = mask_region(psx.cpu.current_pc());

        if self.log_bios_calls {
            bios::check_bios_call(psx);
        }

        // Check if stepping was requested or if we encountered a breakpoint
        if self.step || self.breakpoints.contains(&pc) {
            self.debug(psx);
        }
    }

    /// Called by the psx when it's about to load a value from memory.
    fn memory_read(&mut self, psx: &mut Psx, addr: u32) {
        let addr = mask_region(addr);

        // XXX: how should we handle unaligned watchpoints? For instance if we have a watchpoint on
        // address 1 and the psx executes a `load32 at` address 0, should we break? Also, should we
        // mask the region?
        if self.read_watchpoints.contains(&addr) {
            info!(
                "Read watchpoint triggered at 0x{:08x} [PC=0x{:x}]",
                addr,
                psx.cpu.current_pc()
            );
            self.debug(psx);
        }
    }

    /// Called by the psx when it's about to write a value to memory.
    fn memory_write(&mut self, psx: &mut Psx, addr: u32) {
        let addr = mask_region(addr);

        // XXX: same remark as memory_read for unaligned stores
        if self.write_watchpoints.contains(&addr) {
            info!(
                "Write watchpoint triggered at 0x{:08x} [PC=0x{:x}]",
                addr,
                psx.cpu.current_pc()
            );
            self.debug(psx);
        }
    }
}
