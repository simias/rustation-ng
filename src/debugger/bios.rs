use crate::psx::map::mask_region;
use crate::psx::Psx;

/// Called every time the PC changes when BIOS call logging is enabled
pub fn check_bios_call(psx: &mut Psx) {
    let pc = mask_region(psx.cpu.current_pc());

    if BIOS_VECTOR_ADDR.contains(&pc) {
        // We're in a BIOS vector call
        let vector = pc;
        // $t1 contains the function number
        let func = psx.cpu.regs()[9];
        let ra = psx.cpu.regs()[31];

        let &(name, param_handlers) = match vector {
            0xa0 => vectors::BIOS_VECTOR_A.get(func as usize),
            0xb0 => vectors::BIOS_VECTOR_B.get(func as usize),
            0xc0 => vectors::BIOS_VECTOR_C.get(func as usize),
            _ => None,
        }
        .unwrap_or(&("unknown", &[]));

        let mut params = String::new();
        let mut first = true;

        for (i, ph) in param_handlers.iter().enumerate() {
            // XXX handle stack parameters when needed
            assert!(i <= 3);

            if first {
                first = false;
            } else {
                params.push_str(", ");
            }

            let reg = psx.cpu.regs()[4 + i];

            params.push_str(&ph(psx, reg));
        }

        debug!(
            "BIOS call 0x{:02x}[0x{:02x}](RA = 0x{:08x}): {}({})",
            vector, func, ra, name, params
        );
    }
}

/// The addresses of the three BIOS vectors. In order to call a BIOS function the game sets the
/// function number in R9 before jumping to the function's vector.
const BIOS_VECTOR_ADDR: [u32; 3] = [0xa0, 0xb0, 0xc0];

mod vectors {
    use crate::psx::Psx;

    type ParamHandler = fn(psx: &mut Psx, reg: u32) -> String;

    fn display_char(c: char) -> String {
        match c {
            '\\' => "\\\\".into(),
            '"' => "\"".into(),
            '\'' => "'".into(),
            _ => {
                if c.is_ascii_graphic() || c == ' ' {
                    format!("{}", c)
                } else {
                    match c {
                        '\n' => "\\n".into(),
                        '\t' => "\\t".into(),
                        _ => format!("\\x{:02x}", c as u8),
                    }
                }
            }
        }
    }

    fn char_t(_psx: &mut Psx, reg: u32) -> String {
        let c = reg as u8 as char;

        format!("'{}'", display_char(c))
    }

    fn hex(_psx: &mut Psx, reg: u32) -> String {
        format!("0x{:x}", reg)
    }

    fn uint_t(_psx: &mut Psx, reg: u32) -> String {
        format!("{}", reg)
    }

    fn size_t(_psx: &mut Psx, reg: u32) -> String {
        format!("{}", reg)
    }

    fn int_t(_psx: &mut Psx, reg: u32) -> String {
        format!("{}", reg as i32)
    }

    fn ptr(_psx: &mut Psx, reg: u32) -> String {
        if reg == 0 {
            "(null)".into()
        } else {
            format!("&0x{:08x}", reg)
        }
    }

    fn func_ptr(_psx: &mut Psx, reg: u32) -> String {
        if reg == 0 {
            "(null)()".into()
        } else {
            format!("&0x{:08x}()", reg)
        }
    }

    fn cstr(psx: &mut Psx, reg: u32) -> String {
        if reg == 0 {
            return "(null)".into();
        }

        let mut p = reg;
        let mut s = String::new();

        s.push('"');

        /* Limit the size of strings to avoid spamming a huge message for
         * long or buggy strings */
        for _ in 0..32 {
            let b: u8 = psx.examine(p);

            if b == 0 {
                /* End of string */
                s.push('"');
                return s;
            }

            let c = b as char;

            s.push_str(&display_char(c));
            p = p.wrapping_add(1);
        }

        /* Truncate the string*/
        s.push_str("[...]\"");

        s
    }

    fn event_class(_psx: &mut Psx, reg: u32) -> String {
        let c = match reg {
            0..=0xf => format!("MC:{:x}", reg),
            0xf000_0001 => "VBLANK IRQ".into(),
            0xf000_0002 => "GPU IRQ1".into(),
            0xf000_0003 => "CDROM IRQ".into(),
            0xf000_0004 => "DMA IRQ".into(),
            0xf000_0005 => "TIMER0 IRQ".into(),
            0xf000_0006 => "TIMER1/2 IRQ".into(),
            0xf000_0008 => "PADMEMCARD IRQ".into(),
            0xf000_0009 => "SPU IRQ".into(),
            0xf000_000a => "PIO IRQ".into(),
            0xf000_000b => "SIO IRQ".into(),
            0xf000_0010 => "Exception!".into(),
            0xf000_0011 => "MEMCARD event 1".into(),
            0xf000_0012 => "MEMCARD event 2".into(),
            0xf000_0013 => "MEMCARD event 3".into(),

            0xf200_0000 => "PCLK counter ".into(),
            0xf200_0001 => "HBLANK counter".into(),
            0xf200_0002 => "SYSCLK/8 counter".into(),
            0xf200_0003 => "VBLANK counter".into(),

            0xf300_0000..=0xf3ff_ffff => format!("User event 0x{:x}", reg & 0xff_ffff),

            0xf400_0001 => "MEMCARD BIOS event".into(),
            0xf400_0002 => "Libmath event".into(),

            0xff00_0000..=0xffff_ffff => format!("Thread event 0x{:x}", reg & 0xff_ffff),

            _ => format!("UNKNOWN EVENT 0x{:x}", reg),
        };

        format!("Class {} [0x{:x}]", c, reg)
    }

    fn event_spec(_psx: &mut Psx, reg: u32) -> String {
        // XXX This looks like it should be a bitfield but I'm not sure that it is? In particular
        // codes like 0x8001 make no sense.
        let spec = match reg {
            0x0001 => "Counter is 0",
            0x0002 => "Interrupt",
            0x0004 => "End of I/O",
            0x0008 => "File closed",
            0x0010 => "Command acknowledged",
            0x0020 => "Command completed",
            0x0040 => "Data ready",
            0x0080 => "Data end",
            0x0100 => "Timeout",
            0x0200 => "Unknown command",
            0x0400 => "End of read buffer",
            0x0800 => "End of write buffer",
            0x1000 => "General interrupt",
            0x2000 => "New device",
            0x4000 => "System call",
            0x8000 => "Error",
            0x8001 => "Write error",
            0x0301 => "Libmath domain error",
            0x0302 => "Libmath range error",
            _ => "Unknown",
        };

        format!("Spec {} [0x{:x}]", spec, reg)
    }

    fn void(_psx: &mut Psx, _reg: u32) -> String {
        "void".into()
    }

    /// BIOS vector A functions, lifted from No$
    pub static BIOS_VECTOR_A: [(&str, &[ParamHandler]); 0xb5] = [
        ("FileOpen", &[cstr, hex]),
        ("FileSeek", &[int_t, hex, hex]),
        ("FileRead", &[int_t, ptr, hex]),
        ("FileWrite", &[int_t, cstr, hex]),
        ("FileClose", &[int_t]),
        ("FileIoctl", &[int_t, hex, hex]),
        ("exit", &[uint_t]),
        ("FileGetDeviceFlag", &[int_t]),
        ("FileGetc", &[int_t]),
        ("FilePutc", &[char_t, int_t]),
        ("todigit", &[char_t]),
        ("atof", &[cstr]),
        ("strtoul", &[cstr, ptr, int_t]),
        ("strtol", &[cstr, ptr, int_t]),
        ("abs", &[int_t]),
        ("labs", &[int_t]),
        ("atoi", &[cstr]),
        ("atol", &[cstr]),
        ("atob", &[cstr, ptr]),
        ("SaveState", &[ptr]),
        ("RestoreState", &[ptr, uint_t]),
        ("strcat", &[cstr, cstr]),
        ("strncat", &[cstr, cstr, size_t]),
        ("strcmp", &[cstr, cstr]),
        ("strncmp", &[cstr, cstr, size_t]),
        ("strcpy", &[ptr, cstr]),
        ("strncpy", &[ptr, cstr, size_t]),
        ("strlen", &[cstr]),
        ("index", &[cstr, char_t]),
        ("rindex", &[cstr, char_t]),
        ("strchr", &[cstr, char_t]),
        ("strrchr", &[cstr, char_t]),
        ("strpbrk", &[cstr, ptr]),
        ("strspn", &[cstr, ptr]),
        ("strcspn", &[cstr, ptr]),
        ("strtok", &[cstr, ptr]),
        ("strstr", &[cstr, cstr]),
        ("toupper", &[char_t]),
        ("tolower", &[char_t]),
        ("bcopy", &[ptr, ptr, hex]),
        ("bzero", &[ptr, hex]),
        ("bcmp", &[ptr, ptr, size_t]),
        ("memcpy", &[ptr, ptr, size_t]),
        ("memset", &[ptr, char_t, size_t]),
        ("memmove", &[ptr, ptr, size_t]),
        ("memcmp", &[ptr, ptr, size_t]),
        ("memchr", &[ptr, char_t, size_t]),
        ("rand", &[void]),
        ("srand", &[uint_t]),
        ("qsort", &[ptr, size_t, size_t, func_ptr]),
        ("strtod", &[cstr, ptr]),
        ("malloc", &[size_t]),
        ("free", &[ptr]),
        ("lsearch", &[ptr, ptr, ptr, size_t, func_ptr]),
        ("bsearch", &[ptr, ptr, size_t, size_t, func_ptr]),
        ("calloc", &[size_t, size_t]),
        ("realloc", &[ptr, size_t]),
        ("InitHeap", &[hex, size_t]),
        ("SystemErrorExit", &[uint_t]),
        ("std_in_getchar", &[void]),
        ("std_out_putchar", &[char_t]),
        ("std_in_gets", &[ptr]),
        ("std_out_puts", &[cstr]),
        ("printf", &[cstr]),
        ("SystemErrorUnresolvedException", &[void]),
        ("LoadExeHeader", &[cstr, ptr]),
        ("LoadExeFile", &[cstr, ptr]),
        ("DoExecute", &[ptr, hex, hex]),
        ("FlushCache", &[void]),
        ("init_a0_b0_c0_vectors", &[void]),
        ("GPU_dw", &[uint_t, uint_t, uint_t, uint_t, ptr]),
        ("gpu_send_dma", &[uint_t, uint_t, uint_t, uint_t, ptr]),
        ("SendGP1Command", &[hex]),
        ("GPU_cw", &[hex]),
        ("GPU_cwp", &[ptr, size_t]),
        ("send_gpu_linked_list", &[ptr]),
        ("gpu_abort_dma", &[void]),
        ("GetGPUStatus", &[void]),
        ("gpu_sync", &[void]),
        ("SystemError", &[]),
        ("SystemError", &[]),
        ("LoadAndExecute", &[cstr, hex, hex]),
        ("GetSysSp", &[void]),
        ("SystemError", &[]),
        ("CdInit", &[void]),
        ("_bu_init", &[void]),
        ("CdRemove", &[void]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("dev_tty_init", &[void]),
        ("dev_tty_open", &[uint_t, cstr, hex]),
        ("dev_tty_in_out", &[uint_t, hex]),
        ("dev_tty_ioctl", &[uint_t, hex, hex]),
        ("dev_cd_open", &[uint_t, cstr, hex]),
        ("dev_cd_read", &[uint_t, ptr, size_t]),
        ("dev_cd_close", &[uint_t]),
        ("dev_cd_firstfile", &[uint_t, cstr, hex]),
        ("dev_cd_nextfile", &[uint_t, uint_t]),
        ("dev_cd_chdir", &[uint_t, cstr]),
        ("dev_card_open", &[uint_t, cstr, hex]),
        ("dev_card_read", &[uint_t, ptr, size_t]),
        ("dev_card_write", &[uint_t, ptr, size_t]),
        ("dev_card_close", &[uint_t]),
        ("dev_card_firstfile", &[uint_t, cstr, hex]),
        ("dev_card_nextfile", &[uint_t, uint_t]),
        ("dev_card_erase", &[uint_t, cstr]),
        ("dev_card_undelete", &[uint_t, cstr]),
        ("dev_card_format", &[uint_t]),
        ("dev_card_rename", &[uint_t, cstr, uint_t, cstr]),
        ("unknown", &[]),
        ("_bu_init", &[void]),
        ("CdInit", &[void]),
        ("CdRemove", &[void]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("CdAsyncSeekL", &[ptr]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("CdAsyncGetStatus", &[ptr]),
        ("unknown", &[]),
        ("CdAsyncReadSector", &[uint_t, ptr, hex]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("CdAsyncSetMode", &[hex]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("CdromIoIrqFunc1", &[void]),
        ("CdromDmaIrqFunc1", &[void]),
        ("CdromIoIrqFunc2", &[void]),
        ("CdromDmaIrqFunc2", &[void]),
        ("CdromGetInt5errCode", &[ptr, ptr]),
        ("CdInitSubFunc", &[void]),
        ("AddCDROMDevice", &[void]),
        ("AddMemCardDevice", &[void]),
        ("AddDuartTtyDevice", &[void]),
        ("AddDummyTtyDevice", &[void]),
        ("SystemError", &[]),
        ("SystemError", &[]),
        ("SetConf", &[uint_t, uint_t, ptr]),
        ("GetConf", &[ptr, ptr, ptr]),
        ("SetCdromIrqAutoAbort", &[uint_t, hex]),
        ("SetMemSize", &[uint_t]),
        ("WarmBoot", &[void]),
        ("SystemErrorBootOrDiskFailure", &[cstr, hex]),
        ("EnqueueCdIntr", &[void]),
        ("DequeueCdIntr", &[void]),
        ("CdGetLbn", &[cstr]),
        ("CdReadSector", &[size_t, uint_t, ptr]),
        ("CdGetStatus", &[void]),
        ("bu_callback_okay", &[]),
        ("bu_callback_err_write", &[]),
        ("bu_callback_err_busy", &[]),
        ("bu_callback_err_eject", &[]),
        ("_card_info", &[uint_t]),
        ("_card_async_load_directory", &[uint_t]),
        ("set_card_auto_format", &[hex]),
        ("bu_callback_err_prev_write", &[]),
        ("card_write_test", &[uint_t]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("ioabort_raw", &[uint_t]),
        ("unknown", &[]),
        ("GetSystemInfo", &[hex]),
    ];

    /// BIOS vector B functions, lifted from No$
    pub static BIOS_VECTOR_B: [(&str, &[ParamHandler]); 0x5e] = [
        ("alloc_kernel_memory", &[size_t]),
        ("free_kernel_memory", &[ptr]),
        ("init_timer", &[uint_t, hex, hex]),
        ("get_timer", &[uint_t]),
        ("enable_timer_irq", &[uint_t]),
        ("disable_timer_irq", &[uint_t]),
        ("restart_timer", &[uint_t]),
        ("DeliverEvent", &[event_class, event_spec]),
        ("OpenEvent", &[event_class, event_spec, hex, func_ptr]),
        ("CloseEvent", &[uint_t]),
        ("WaitEvent", &[uint_t]),
        ("TestEvent", &[uint_t]),
        ("EnableEvent", &[uint_t]),
        ("DisableEvent", &[uint_t]),
        ("OpenThread", &[ptr, ptr, ptr]),
        ("CloseThread", &[ptr]),
        ("ChangeThread", &[ptr]),
        ("unknown", &[]),
        ("InitPad", &[ptr, size_t, ptr, size_t]),
        ("StartPad", &[void]),
        ("StopPad", &[void]),
        ("OutdatedPadInitAndStart", &[hex, ptr, hex, hex]),
        ("OutdatedPadGetButtons", &[void]),
        ("ReturnFromException", &[void]),
        ("SetDefaultExitFromException", &[void]),
        ("SetCustomExitFromException", &[ptr]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("UnDeliverEvent", &[event_class, event_spec]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("FileOpen", &[cstr, hex]),
        ("FileSeek", &[uint_t, size_t, hex]),
        ("FileRead", &[uint_t, ptr, size_t]),
        ("FileWrite", &[uint_t, ptr, size_t]),
        ("FileClose", &[uint_t]),
        ("FileIoctl", &[uint_t, hex, hex]),
        ("exit", &[uint_t]),
        ("FileGetDeviceFlag", &[uint_t]),
        ("FileGetc", &[uint_t]),
        ("FilePutc", &[char_t, uint_t]),
        ("std_in_getchar", &[void]),
        ("std_out_putchar", &[char_t]),
        ("std_in_gets", &[ptr]),
        ("std_out_puts", &[ptr]),
        ("chdir", &[cstr]),
        ("FormatDevice", &[cstr]),
        ("firstfile", &[cstr, hex]),
        ("nextfile", &[cstr, hex]),
        ("FileRename", &[cstr, cstr]),
        ("FileDelete", &[cstr]),
        ("FileUndelete", &[cstr]),
        ("AddDevice", &[ptr]),
        ("RemoveDevice", &[cstr]),
        ("PrintInstalledDevices", &[void]),
        ("InitCard", &[hex]),
        ("StartCard", &[void]),
        ("StopCard", &[void]),
        ("_card_info_subfunc", &[uint_t]),
        ("write_card_sector", &[uint_t, uint_t, ptr]),
        ("read_card_sector", &[uint_t, uint_t, ptr]),
        ("allow_new_card", &[void]),
        ("Krom2RawAdd", &[hex]),
        ("SystemError", &[]),
        ("Krom2Offset", &[hex]),
        ("GetLastError", &[void]),
        ("GetLastFileError", &[uint_t]),
        ("GetC0Table", &[void]),
        ("GetB0Table", &[void]),
        ("get_bu_callback_port", &[void]),
        ("testdevice", &[cstr]),
        ("SystemError", &[]),
        ("ChangeClearPad", &[uint_t]),
        ("get_card_status", &[uint_t]),
        ("wait_card_status", &[uint_t]),
    ];

    /// BIOS vector C functions, lifted from No$
    pub static BIOS_VECTOR_C: [(&str, &[ParamHandler]); 0x1e] = [
        ("EnqueueTimerAndVblankIrqs", &[uint_t]),
        ("EnqueueSyscallHandler", &[uint_t]),
        ("SysEnqIntRP", &[uint_t, ptr]),
        ("SysDeqIntRP", &[uint_t, ptr]),
        ("get_free_EvCB_slot", &[void]),
        ("get_free_TCB_slot", &[void]),
        ("ExceptionHandler", &[void]),
        ("InstallExceptionHandlers", &[void]),
        ("SysInitMemory", &[ptr, size_t]),
        ("SysInitKernelVariables", &[void]),
        ("ChangeClearRCnt", &[uint_t, hex]),
        ("SystemError", &[]),
        ("InitDefInt", &[uint_t]),
        ("SetIrqAutoAck", &[uint_t, hex]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("unknown", &[]),
        ("InstallDevices", &[hex]),
        ("FlushStdInOutPut", &[void]),
        ("unknown", &[]),
        ("tty_cdevinput", &[uint_t, char_t]),
        ("tty_cdevscan", &[void]),
        ("tty_circgetc", &[uint_t]),
        ("tty_circputc", &[char_t, uint_t]),
        ("ioabort", &[cstr, cstr]),
        ("set_card_find_mode", &[hex]),
        ("KernelRedirect", &[hex]),
        ("AdjustA0Table", &[void]),
        ("get_card_find_mode", &[void]),
    ];
}
