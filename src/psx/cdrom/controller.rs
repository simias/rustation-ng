//! Implementation of the CD drive microcontroller

use std::cmp::max;

use super::disc;
use super::disc::Disc;
use super::resampler::AudioResampler;
use super::simple_rand::SimpleRand;
use super::Fifo;
use crate::psx::cpu::CPU_FREQ_HZ;
use crate::psx::{CycleCount, Psx};

use cdimage::bcd::Bcd;
use cdimage::msf::Msf;
use cdimage::sector::{Sector, XaBitsPerSample, XaCodingInfo, XaForm, XaSamplingFreq};
use cdimage::TrackFormat;

type AsyncResponse = fn(&mut Psx) -> CycleCount;

/// Controller state. This is an 8bit controller in charge of the disc drive.
pub struct Controller {
    /// Currently loaded disc or None if the tray is empty
    disc: Option<Disc>,
    /// Controller state machine
    sequence: ControllerSequence,
    /// Countdown until the next step in `sequence`
    sequence_timer: CycleCount,
    /// Internal command parameter FIFO
    params: Fifo,
    /// Internal command response FIFO
    response: Fifo,
    /// Status for the current command
    irq_code: IrqCode,
    /// Async command response. The tuple contains a method pointer to
    /// the asynchronous command handler and the number of CPU cycles
    /// until the asynch handler must be run.
    async_response: Option<(CycleCount, AsyncResponse)>,
    /// Variable holding the drive state
    drive_state: DriveState,
    /// Contains the number of cycles until the next sector read (if any). Note that even when
    /// paused the drive continuously reads sectors, it just handles them differently depending on
    /// the current state.
    next_sector: Option<CycleCount>,
    /// Holds a pending asynchronous notification (e.g. sector read), if any
    pending_notification: Option<IrqCode>,
    /// Target of the last seek command
    seek_target: Msf,
    /// Target of the next seek command
    next_seek_target: Msf,
    /// True if `next_seek_target` has been modified but no seek has taken place
    seek_target_pending: bool,
    /// Current read position
    position: Msf,
    /// If true the drive is in double speed mode (2x, 150 sectors per second), otherwise we're in
    /// the default 1x (75 sectors per second).
    double_speed: bool,
    /// If true Send ADPCM samples to the SPU
    xa_adpcm_to_spu: bool,
    /// If true we read the whole sector except for the sync bytes (0x924 bytes), otherwise it only
    /// reads 0x800 bytes.
    read_whole_sector: bool,
    /// Not sure what this does exactly, apparently it overrides the normal sector size. Needs to
    /// run more tests to see what it does exactly.
    sector_size_override: bool,
    /// Enable CD-DA mode to play Redbook Audio tracks
    cdda_mode: bool,
    /// If true automatically pause at the end of the track
    autopause: bool,
    /// If true the controller will generate interrupts for each sector while playing CD-DA tracks.
    /// The response will contain the current location amongst other things.
    report_interrupts: bool,
    /// True if the ADPCM filter is enabled
    filter_enabled: bool,
    /// If ADPCM filtering is enabled only sectors with this file number are processed
    filter_file: u8,
    /// If ADPCM filtering is enabled only sectors with this channel number are processed
    filter_channel: u8,
    /// When streaming audio to the SPU and `filter_enabled` is false this variable contains the
    /// first file and channels numbers encountered. It's set to Some(0, 0) when the EOF sector is
    /// encountered
    filter_last: Option<(u8, u8)>,
    /// The last two previous decoded ADPCM samples for the left and right channels
    adpcm_last: [[i16; 2]; 2],
    /// Frequency of the audio in audio_buffer
    audio_frequency: AudioFrequency,
    /// Buffer containing the stereo audio samples when we're streaming to the SPU.
    audio_buffer: Vec<[i16; 2]>,
    /// Current read index into `audio_buffer`
    audio_index: u32,
    /// Current phase for audio resampling, in 1/7th of a sample. We need this because the XA ADPCM
    /// streams have a frequency of 37.8kHz or 18.9kHz, respectively 6/7 and 3/7 the CD-DA audio
    /// frequency of 44.1kHz
    audio_phase: u8,
    /// Audio resamplers for the left and right channels
    audio_resamplers: [AudioResampler; 2],
    /// PRNG to simulate the pseudo-random CD controller timings (from the host's perspective)
    rand: SimpleRand,
    /// Last raw sector read from the disc image
    sector: Sector,
}

impl Controller {
    pub fn new(disc: Option<Disc>) -> Controller {
        // Put start position at beginning of track 01 (at INDEX 01, after the pregap)
        //
        // XXX I'm not entirely sure what the real value is at startup (or after stop/reset
        // commands, which is potentially more annoying)
        let position = Msf::from_bcd(0x00, 0x02, 0x00).unwrap();

        Controller {
            disc,
            drive_state: DriveState::Idle,
            next_sector: None,
            pending_notification: None,
            sequence: ControllerSequence::Idle,
            sequence_timer: 0,
            params: Fifo::new(),
            response: Fifo::new(),
            irq_code: IrqCode::Ok,
            async_response: None,
            seek_target: position,
            next_seek_target: Msf::zero(),
            seek_target_pending: false,
            position,
            double_speed: false,
            xa_adpcm_to_spu: false,
            read_whole_sector: true,
            sector_size_override: false,
            cdda_mode: false,
            autopause: false,
            report_interrupts: false,
            filter_enabled: false,
            filter_file: 0,
            filter_channel: 0,
            filter_last: None,
            adpcm_last: [[0; 2]; 2],
            audio_frequency: AudioFrequency::Da1x,
            audio_buffer: Vec::with_capacity(4096),
            audio_index: 0,
            audio_phase: 0,
            audio_resamplers: [AudioResampler::new(), AudioResampler::new()],
            rand: SimpleRand::new(),
            sector: Sector::empty(),
        }
    }

    /// Assembles the first status byte returned by many commands
    fn drive_status(&self) -> u8 {
        if self.disc.is_none() {
            // No disc, pretend that the shell is open (bit 4)
            return 0x10;
        }

        let motor_on = 1 << 1;

        // XXX on the real hardware bit 4 is always set the first time this command is called
        // even if the console is booted with the tray closed. Using the "get_stat" command
        // command clears it however.
        match self.drive_state {
            DriveState::Idle => 0,
            DriveState::Reading => motor_on | (1 << 5),
            DriveState::Seeking(_, _) => motor_on | (1 << 6),
            DriveState::Paused => motor_on,
            DriveState::ReadToc => motor_on, // XXX not sure about this one
            DriveState::Playing => motor_on | (1 << 7),
        }
    }

    /// Push the drive status in the response FIFO
    fn push_drive_status(&mut self) {
        let status = self.drive_status();
        self.response.push(status);
    }

    /// Returns the status with the error bit set
    fn drive_error(&self) -> u8 {
        self.drive_status() | 1
    }

    /// Push the drive status in the response FIFO
    fn push_drive_error(&mut self) {
        let status = self.drive_error();
        self.response.push(status);
    }

    pub fn start_command(&mut self) {
        debug_assert!(!self.in_command());

        if self.async_command_pending() {
            // Not sure what's supposed to happen here, might be command dependant. Can't really
            // see why anybody would want to start a new command without waiting for the response
            // to the previous one though.
            panic!("New CD command while still waiting for an async response");
        }

        self.sequence = ControllerSequence::CommandPending;
        self.params.clear();
        self.response.clear();

        // Let's compute the seemingly pseudo-random command pending delay. Of course in reality
        // the delay is not truly random, it depends on the controller's internal state but since
        // we don't LLE it this is the best we can do right now.
        let min = timings::COMMAND_PENDING.0 as u32;
        let max = timings::COMMAND_PENDING.1 as u32;
        self.sequence_timer = self.rand.get(min, max) as CycleCount;

        // Assume the command will be succesful, let the command
        // handler override that if something goes wrong.
        self.irq_code = IrqCode::Ok;
    }

    pub fn schedule_async_response(&mut self, delay: CycleCount, handler: AsyncResponse) {
        assert!(self.async_response.is_none());

        self.async_response = Some((delay, handler));
    }

    /// Return true if the sub-CPU is executing a command
    pub fn in_command(&self) -> bool {
        self.sequence != ControllerSequence::Idle
    }

    /// Return true if an async command is pending
    pub fn async_command_pending(&self) -> bool {
        self.async_response.is_some()
    }

    /// Busy flag state. This is *not* equivalent to `in_command()`, the busy flag goes down
    /// roughly 2000 cyles before the IRQ triggers.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.sequence,
            ControllerSequence::CommandPending
                | ControllerSequence::ParamPush
                | ControllerSequence::Execution
                | ControllerSequence::RxFlush
                | ControllerSequence::RxPush
                | ControllerSequence::BusyDelay
        )
    }

    pub fn get_async_response(&self) -> Option<(CycleCount, AsyncResponse)> {
        self.async_response
    }

    pub fn clear_response(&mut self) {
        self.async_response = None;
        self.response.clear();

        // Assume the next command is going to be successful, the handler is free to override this
        // value if it's not
        self.irq_code = IrqCode::AsyncOk;
    }

    /// Queue a notification that will be triggered as soon as no other events are pending
    pub fn notify(&mut self, code: IrqCode) {
        if let Some(n) = self.pending_notification {
            // I suppose the previous one is simply discarded but requires more tests
            unimplemented!(
                "Notification-within-notification! ({:?} while {:?}",
                code,
                n
            );
        }

        self.pending_notification = Some(code);
    }

    pub fn maybe_process_notification(&mut self) {
        if let Some(irq) = self.pending_notification {
            if self.in_command() {
                // Can't perform notification now (I think? Otherwise the software won't be able to
                // differentiate between a notification and a command result).
                return;
            }

            self.irq_code = irq;

            self.response.clear();
            // XXX Is it the drive status at the moment the notification is triggered or when the
            // notification was first queued?
            self.push_drive_status();

            self.sequence = ControllerSequence::AsyncRxPush;
            self.sequence_timer = timings::READ_RX_PUSH;
            self.pending_notification = None;
        }
    }

    /// Execute a pending seek (if any). On the real console that would mean physically moving the
    /// read head. Returns the estimated seek time.
    fn do_seek(&mut self, seek_type: SeekType, after_seek: AfterSeek) -> CycleCount {
        self.seek_target = self.next_seek_target;
        self.seek_target_pending = false;
        self.drive_state = DriveState::Seeking(seek_type, after_seek);

        let mut seek_time = self.estimate_seek_time();

        if after_seek == AfterSeek::Read {
            // XXX That's what Mefnaden does. Wouldn't it make more sense to be seeking for the
            // normal amount of time and *then* switch to Read mode for one sector worth of time
            // before we get the actual sector data ?
            seek_time += self.cycles_per_sector();
        }

        self.next_sector = Some(seek_time);

        self.position = self.seek_target;

        seek_time
    }

    /// Estimate the amount of time to move from the current sector to the seek target.
    fn estimate_seek_time(&mut self) -> CycleCount {
        let mut t = 0;

        // This code is based on Mednafen's implementation. It seems very complicated for such a
        // rough estimate but it's probably a good place to start.
        let cur_index = if self.drive_state == DriveState::Idle {
            // Motor is off, 1s seek penalty
            t += CPU_FREQ_HZ;
            0
        } else {
            self.position.sector_index() as i32
        };

        let target_index = self.seek_target.sector_index() as i32;

        let off = (target_index - cur_index).abs();

        // ~3us seek time per sector. I assume that the idea here is that seeking through an entire
        // disc would be estimated to take 1s. What's weird is that as far as I know a standard
        // disc is 74 minutes, not 72. Maybe a typo? Or maybe it was really tested with a 72min
        // disc. It won't make a massive difference in practice.
        let t_off = (off as i64 * CPU_FREQ_HZ as i64 * 1000 / (72 * 60 * 75)) / 1000;

        t += max(t_off as i32, 20_000);

        if off >= 2250 {
            // Add ~0.3s seek time
            t += (CPU_FREQ_HZ * 3) / 10;
        } else if self.drive_state == DriveState::Paused {
            // XXX there's a difference with Mednafen's code here: our "paused" state is different
            // from theirs because they also have a "standby" mode which is also included in our
            // "paused". I don't really understand what this "standby" mode is and as far as I can
            // tell the only place in Mednafen's code where it's handled differently than "paused"
            // is this very seek time algorithm. I don't know if it's an error or if there's a real
            // difference.
            //
            // TODO: figure out if there's really a "standby" state different from "paused".
            t += 2_475_904 >> (self.double_speed as u32)
        } else if (3..12).contains(&off) {
            t += self.cycles_per_sector() << 2;
        }

        t += self.rand.get(0, 25_000) as CycleCount;

        t
    }

    /// Return the number of CPU cycles needed to read a single sector depending on the current
    /// drive speed. The PSX drive can read 75 sectors per second at 1x or 150sectors per second at
    /// 2x.
    fn cycles_per_sector(&self) -> CycleCount {
        // 1x speed: 75 sectors per second
        let cycles_1x = CPU_FREQ_HZ / 75;

        cycles_1x >> (self.double_speed as u32)
    }

    /// Read the sector at the current `self.position` into `self.sector`
    fn read_sector_at_position(&mut self) {
        // XXX the real hardware seems to have some kind of "FIFO", where you (sometimes?) get a
        // small delay between the sector being read from and it becoming available to software.
        let position = self.position;

        // Read the sector at `position`
        let sector = match self.disc {
            Some(ref mut d) => d.image().read_sector(position),
            None => unimplemented!("Sector read without a disc"),
        };

        self.sector = match sector {
            Ok(s) => s.clone(),
            Err(e) => unimplemented!("Couldn't read sector {}: {}", position, e),
        };
    }

    pub fn sector_data(&mut self) -> &[u8] {
        // Extract the data we need from the sector.
        if self.read_whole_sector {
            // Read the entire sector except for the 12bits sync pattern

            let data = match self.sector.data_2352() {
                Ok(d) => d,
                Err(e) => panic!("Failed to read whole sector {}", e),
            };

            // Skip the sync pattern
            &data[12..]
        } else {
            // Read 2048 bytes after the Mode2 XA sub-header
            let data = match self.sector.mode2_xa_payload() {
                Ok(d) => d as &[u8],
                Err(e) => panic!("Failed to read sector: {}", e),
            };

            if data.len() > 2048 {
                // This is a Mode 2 Form 2 sector, it has more
                // data and no error correction. It probably
                // shouldn't be read without
                // `read_whole_sector` being set.
                warn!("Form 2 sector partial read");
            }

            &data[0..2048]
        }
    }

    /// Handle sector read and retuns the time to the next sector (if any)
    fn handle_sector_read(&mut self) -> Option<CycleCount> {
        self.read_sector_at_position();

        // Set to false if the new sector isn't meant to be send to the software and we shouldn't
        // notify the read
        let mut for_software = true;

        // Update state machine
        match self.drive_state {
            DriveState::Idle => unreachable!(),
            DriveState::ReadToc => unreachable!(),
            DriveState::Seeking(seek_type, after_seek) => {
                // XXX If I understand correctly SeekType::Data bases itself on the data track
                // sector's embedded MSF to locate its target (per No$). So I assume that
                // technically we should be scanning the data segments to find the target. It makes
                // sense: if you're a little off for audio tracks it's not usually a big deal, for
                // data you want to make sure you're on the right sector.
                //
                // Mednafen implements this by seeking to the seek_target, checking the sector's
                // MSF and if it's not right keep reading ahead for a maximum of 128 sectors
                // looking for the real target and then the seek completes.
                //
                // Of course in our situation unless the CD image is messed up we should always
                // land "perfectly", so I can't think of any reason for us to ever miss the target.
                // Mednafen mentions that "Rockman complete works (at least 2 and 4)" have "finicky
                // fubared CD access code" (which I don't really understand, the CD image would
                // have to be fubared, not the access code?) so it's probably worth testing those
                // if we ever want to implement that correctly.
                if seek_type == SeekType::Data {
                    let cdrom_header = match self.sector.cd_rom_header() {
                        Ok(h) => h,
                        Err(e) => unimplemented!("Data seek target isn't a CD-ROM sector! {:?}", e),
                    };

                    if cdrom_header.msf != self.position {
                        unimplemented!(
                            "Seek target MSF mismatch! (expected {} got {})",
                            self.position,
                            cdrom_header.msf
                        );
                    }
                }

                self.drive_state = match after_seek {
                    AfterSeek::Read => DriveState::Reading,
                    AfterSeek::Pause => DriveState::Paused,
                    AfterSeek::Play => DriveState::Playing,
                };
            }
            DriveState::Reading => {
                // XXX Test Subchannel Q byte 0 for the "data flag".

                if self.xa_adpcm_to_spu && self.handle_xa_adpcm_sector() {
                    // Sector is meant to be streamed directly to the SPU, don't notify software
                    for_software = false;
                }

                if self.cdda_mode {
                    // XXX According to mednafen is cdda_mode is true and we have an audio sector
                    // (as identified by the Q subchannel), the audio *is* sent to the SPU.
                    unimplemented!("CDDA mode while reading!");
                }
            }
            DriveState::Playing => {
                // XXX Test Subchannel Q byte 0 for lack of "data flag"
                if self.cdda_mode {
                    self.handle_da_sector();
                    for_software = false;
                }
            }
            DriveState::Paused => (),
        }

        let cps = self.cycles_per_sector();

        // 2nd round for the (possibly) new state in order to figure out if we need to keep reading
        // or not
        match self.drive_state {
            DriveState::ReadToc => unreachable!(),
            DriveState::Seeking(_, _) => unreachable!(),
            DriveState::Idle => None,
            DriveState::Paused => {
                self.position = self.position.next().unwrap();

                // The read head hovers around the seek target
                // XXX These values are taken from mednafen
                let pos = self.position.sector_index() as i32;
                let next = self.seek_target.sector_index() as i32;

                if pos > next + 1 {
                    // We're too far ahead, move back
                    let new_pos = max(pos - 10, 0);

                    self.position = Msf::from_sector_index(new_pos as u32).unwrap();
                }

                Some(cps)
            }
            DriveState::Reading => {
                if for_software {
                    self.notify(IrqCode::SectorReady);
                }
                self.position = self.position.next().unwrap();
                Some(cps)
            }
            DriveState::Playing => {
                self.position = self.position.next().unwrap();
                Some(cps)
            }
        }
    }

    /// See if the current sector is meant for XA ADPCM streaming to the SPU. If that's the case,
    /// process it and return `true`, otherwise do nothing and return `false`.
    fn handle_xa_adpcm_sector(&mut self) -> bool {
        let subheader = match self.sector.mode2_xa_subheader() {
            Ok(sub) => sub,
            Err(_) => {
                // Not a Mode2 XA track
                warn!("Non-XA CD-ROM track while streaming to SPU");
                return false;
            }
        };

        // XXX Test based on No$ and Mednafen, I haven't tested this myself
        let sm = subheader.submode();
        let is_audio = sm.audio() && sm.form() == XaForm::Form2 && sm.real_time();

        if !is_audio {
            return false;
        }

        // XXX At this point Mednafen always return true (i.e. consume the sector and doesn't
        // notify the software) even if the track is filtered. No$ seems to disagree slightly
        // because it says that's only true if `filter_enabled` is true.

        let file_no = subheader.file_number();
        let channel_no = subheader.channel_number();

        if self.filter_enabled {
            // See if this sector passes the filter
            let filtered = file_no != self.filter_file || channel_no != self.filter_channel;

            if filtered {
                // Nope, bail out
                return true;
            }
        }

        if let Some((last_file, last_channel)) = self.filter_last {
            // XXX Taken from Mednafen, the logic seems to be that the controller will "stick" to
            // the first file:channel encountered and ignore the rest.
            if file_no != last_file || channel_no != last_channel {
                // Ignore
                return true;
            }
        }

        self.filter_last = Some((file_no, channel_no));

        if sm.end_of_file() {
            // This is also from Mednafen: in practice it does interrupt the playback... unless we
            // have a stream whose file and channels are both 0.
            self.filter_last = Some((0, 0));
        }

        // If we reach this point we need to send the audio to the SPU
        self.decode_xa_adpcm_sector();

        true
    }

    /// We're playing a regular CD-DA audio track, send audio to the SPU
    fn handle_da_sector(&mut self) {
        let format = self.sector.metadata().format;

        if format != TrackFormat::Audio {
            unimplemented!("Got DA sector that's not audio! ({:?})", format);
        }

        // CD-DA is always 2352 bytes per track which gives 588 pairs of 16bit stereo samples.
        self.audio_buffer.resize(2352 / 4, [0, 0]);
        self.audio_index = 0;
        self.audio_frequency = if self.double_speed {
            AudioFrequency::Da2x
        } else {
            AudioFrequency::Da1x
        };

        // Copy the full sector into the audio buffer
        let raw = self.sector.data_2352().unwrap();

        for (n, pair) in self.audio_buffer.iter_mut().enumerate() {
            let raw_pos = n << 2;

            let left_lo = raw[raw_pos] as u16;
            let left_hi = raw[raw_pos + 1] as u16;
            let right_lo = raw[raw_pos + 2] as u16;
            let right_hi = raw[raw_pos + 4] as u16;

            let left = left_lo | (left_hi << 8);
            let right = right_lo | (right_hi << 8);

            pair[0] = left as i16;
            pair[1] = right as i16;
        }
    }

    /// Called when we have an XA ADPCM sector and we must send it to the SPU
    fn decode_xa_adpcm_sector(&mut self) {
        // If we reach this point this shouldn't fail
        let subheader = self.sector.mode2_xa_subheader().unwrap();
        let coding = match subheader.coding_info() {
            XaCodingInfo::Audio(c) => c,
            c => panic!("Unexpected coding info for audio track: {:?}", c),
        };

        let data = self.sector.mode2_xa_payload().unwrap();

        // Each sector contains an "audio block" of 2304B and 20B of padding (that should be 0) for
        // a total of 2324 bytes (the length of an XA Form2 payload)
        debug_assert!(data.len() == 2324);

        let shift_4bpp = match coding.bits_per_sample() {
            XaBitsPerSample::S4Bits => 1,
            XaBitsPerSample::S8Bits => 0,
        };

        let units_per_group = 4 << shift_4bpp;
        let samples_8bpp = shift_4bpp == 0;

        let stereo = coding.stereo();

        // Total number of samples we're about to decode
        let total_samples = 18 * (4 << shift_4bpp) * 28;

        // 1 for stereo, 0 for mono
        let stereo_one = stereo as usize;

        let buffer_len = if stereo {
            total_samples / 2
        } else {
            total_samples
        };

        self.audio_buffer.resize(buffer_len, [0, 0]);
        self.audio_index = 0;

        self.audio_frequency = match coding.sampling_frequency() {
            XaSamplingFreq::F18_9 => {
                // Sampling frequency is 18.9kHz, 3/7 * 44.1kHz
                AudioFrequency::Xa18k9
            }
            XaSamplingFreq::F37_8 => {
                // Sampling frequency is 37.8kHz, 6/7 * 44.1kHz
                AudioFrequency::Xa37k8
            }
        };

        // Offsets in the output buffer, per channel
        let mut output_offsets = [0; 2];

        // Each audio block contains 18 "sound groups" of 128 bytes
        for group in 0..18 {
            let group_off = 128 * group;
            // Each group has a 16 byte "Sound Parameters" header...
            let sp = &data[group_off..group_off + 16];
            // ... and 112 bytes of "Sample Audio Data"
            let audio_data = &data[group_off + 16..group_off + 128];

            // Each group has between 4 and 8 "Sound Units" depenting on the sample bit depth
            for unit in 0..units_per_group {
                // The params are stored twice, the second time at the same address | 4. Not really
                // sure why, if you detect a discrepancy at this point what can you do anyway?
                let param = sp[((unit << 1) & 8) | (unit & 3)];
                let shift = param & 0xf;
                let weights: [(i32, i32); 16] = [
                    (0, 0),
                    (60, 0),
                    (115, -52),
                    (98, -55),
                    (122, -60),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                    (0, 0),
                ];
                let (wp, wn) = weights[(param >> 4) as usize];

                let channel = unit & stereo_one;

                for i in 0..28 {
                    let encoded = if samples_8bpp {
                        audio_data[(i << 2) | unit]
                    } else {
                        // 4bpp: 2 samples per byte
                        let s = audio_data[(i << 2) | (unit >> 1)];

                        // Convert the sample to 8 bit by setting the low 4 bits to 0
                        if unit & 1 == 0 {
                            s << 4
                        } else {
                            s & 0xf0
                        }
                    };

                    // Convert to signed 16 bits
                    let sample = (u16::from(encoded) << 8) as i16;
                    // Convert to 32bits to handle overflows
                    let mut sample = i32::from(sample);

                    // ADPCM decode
                    sample >>= shift;
                    let sample_1 = i32::from(self.adpcm_last[channel][0]);
                    let sample_2 = i32::from(self.adpcm_last[channel][1]);
                    sample += (sample_1 * wp + sample_2 * wn) >> 6;

                    // Saturate to 16 bits
                    let sample = if sample > i16::max_value() as i32 {
                        i16::max_value()
                    } else if sample < i16::min_value() as i32 {
                        i16::min_value()
                    } else {
                        sample as i16
                    };

                    // Rotate last samples
                    self.adpcm_last[channel][1] = self.adpcm_last[channel][0];
                    self.adpcm_last[channel][0] = sample;

                    // Store the data in the output buffer
                    let sample_off = output_offsets[channel];
                    self.audio_buffer[sample_off][channel] = sample;
                    output_offsets[channel] += 1;
                }
            }
        }

        if !stereo {
            // Recopy the left channel to the right
            for pair in &mut self.audio_buffer {
                pair[1] = pair[0];
            }
        }
    }

    /// Called at 44.1kHz to advance our audio state machine and return a new sample. If `resample`
    /// is true we resample if necessary, otherwise we just return the newest sample
    pub fn run_audio_cycle(&mut self, resample: bool) -> (i16, i16) {
        let cur_index = self.audio_index as usize;

        // Next sample waiting to be output
        let &[first_left, first_right] = match self.audio_buffer.get(cur_index) {
            Some(stereo) => stereo,
            // No data left in the buffer, it normally means that we're not currently streaming any
            // audio but it could also happen if we end up starving during playback.
            //
            // If the latter happens that could either mean buggy emulator code or a broken CD
            // image (missing audio sectors for instance) or broken game code (bad streaming
            // configuration, disc speed too slow etc...).
            None => return (0, 0),
        };

        // Value returned when no resampling is needed
        let mut left = first_left;
        let mut right = first_right;

        match self.audio_frequency {
            AudioFrequency::Da2x => {
                // We're running at 2 * 44.1kHz, that means that we run at twice the SPU audio
                // frequency and must skip every other sample.
                //
                // XXX Mednafen doesn't resample here, should we? Do games actually use this mode
                // for some reason?
                self.audio_index += 2;
            }
            AudioFrequency::Da1x => {
                // We're running at 44.1kHz, in other words we're running at the normal CD-DA
                // frequency and we can just return one sample every time
                self.audio_index += 1;
            }
            AudioFrequency::Xa18k9 | AudioFrequency::Xa37k8 => {
                // We're running at a fraction of 44.1kHz, we need to resample
                if resample {
                    left = self.audio_resamplers[0].resample(self.audio_phase);
                    right = self.audio_resamplers[1].resample(self.audio_phase);
                }

                // This value is the ratio of the audio frequency to the output frequency of
                // 44.1kHz in multiples of 1/7th. So for instance for Xa37k8 the input frequency
                // is 37.8kHz so phase step will be 6 because 37.8kHz = 6/7 * 44.1kHz
                let phase_step = self.audio_frequency as u8;

                // Advance to the next step
                self.audio_phase += phase_step;
                if self.audio_phase >= 7 {
                    self.audio_phase -= 7;
                    // Consume one sample
                    self.audio_resamplers[0].push_sample(first_left);
                    self.audio_resamplers[1].push_sample(first_right);
                    self.audio_index += 1;
                }
            }
        }

        (left, right)
    }

    /// Called when audio streaming to the SPU should be restarted
    fn restart_audio(&mut self) {
        self.filter_last = None;
        self.adpcm_last = [[0; 2]; 2];
        self.audio_buffer.truncate(0);
        self.audio_phase = 0;
        self.audio_resamplers[0].restart();
        self.audio_resamplers[1].restart();
    }

    /// Returns the start (INDEX01) of the given track in the disc, or None if the track doesn't
    /// exist
    fn get_track_start(&mut self, track: Bcd) -> Option<Msf> {
        match self.disc {
            Some(ref mut disc) => {
                let image = disc.image();
                let toc = image.toc();
                let tracks = toc.tracks();

                tracks.iter().find(|t| t.track == track).map(|t| t.start)
            }
            None => None,
        }
    }
}

pub fn run(psx: &mut Psx, cycles: CycleCount) {
    // Check for sector reads
    if let Some(delay) = psx.cdrom.controller.next_sector {
        let next = if delay > cycles {
            Some(delay - cycles)
        } else {
            let leftover = cycles - delay;

            let next = match psx.cdrom.controller.handle_sector_read() {
                Some(next) => {
                    // If this triggers something very fishy is happening, it means that we
                    // missed a sync and ran very far ahead
                    if next <= leftover {
                        panic!("{} > {}", next, leftover);
                    }

                    debug_assert!(next > leftover);
                    Some(next - leftover)
                }
                None => None,
            };

            super::maybe_process_notification(psx);

            next
        };

        psx.cdrom.controller.next_sector = next;
    }

    // XXX I don't care for "leftover" cycles like above because command timings are very rough
    // anyway. Cycle reads need to be precise because otherwise they can mess up audio playback.
    if psx.cdrom.controller.in_command() {
        let sequence_timer = psx.cdrom.controller.sequence_timer;
        if sequence_timer > cycles {
            // We haven't reached the next step yet
            psx.cdrom.controller.sequence_timer -= cycles;
        } else {
            // We reached the end of the current step, advance until the beginning of the next
            // one.
            next_controller_step(psx);
        }
    } else if let Some((delay, handler)) = psx.cdrom.controller.async_response {
        if delay > cycles {
            psx.cdrom.controller.async_response = Some((delay - cycles, handler));
        } else {
            // The async event is ready to be processed
            psx.cdrom.controller.async_response = Some((0, handler));
            super::maybe_process_async_response(psx);
        }
    }
}

pub fn predict_next_sync(psx: &mut Psx) -> CycleCount {
    let controller = &mut psx.cdrom.controller;

    // Start with an arbitrarily large value. This value will be used if no event is pending.
    let mut next_sync = 1_000_000;

    // Check for command completion
    if controller.in_command() {
        if controller.sequence_timer < next_sync {
            next_sync = controller.sequence_timer;
        }
    } else {
        // Check for async response.
        if let Some((delay, _)) = controller.async_response {
            // It's possible for delay to be 0 here if there's an async event pending and we have
            // an unacknowledged IRQ
            if delay > 0 && delay < next_sync {
                next_sync = delay;
            }
        }
    }

    // Check for sector read
    if let Some(delay) = controller.next_sector {
        if delay < next_sync {
            next_sync = delay;
        }
    }

    next_sync
}

fn next_controller_step(psx: &mut Psx) {
    let (next_step, duration) = match psx.cdrom.controller.sequence {
        // We shouldn't be called if we're idle
        ControllerSequence::Idle => unreachable!(),
        ControllerSequence::CommandPending | ControllerSequence::ParamPush => {
            if psx.cdrom.host_params.is_empty() {
                // No parameters left to transmit from the host FIFO, we can run the command

                // If we got to this point `command` cannot be None, or something very weird is
                // going on
                let command = psx.cdrom.command.unwrap();
                execute_command(psx, command);
                (ControllerSequence::Execution, timings::EXECUTION)
            } else {
                // We have parameters left to send
                let param = psx.cdrom.host_params.pop();
                psx.cdrom.controller.params.push(param);

                (ControllerSequence::ParamPush, timings::PARAM_PUSH)
            }
        }
        ControllerSequence::Execution => {
            psx.cdrom.host_response.clear();
            (ControllerSequence::RxFlush, timings::RX_FLUSH)
        }
        ControllerSequence::RxFlush | ControllerSequence::RxPush => {
            // We know that there is always at least one response byte for any command so we
            // can run this unconditionally after `RxFlush`
            let b = psx.cdrom.controller.response.pop();
            psx.cdrom.host_response.push(b);

            if psx.cdrom.controller.response.is_empty() {
                (ControllerSequence::BusyDelay, timings::BUSY_DELAY)
            } else {
                // We have response bytes left to push
                (ControllerSequence::RxPush, timings::RX_PUSH)
            }
        }
        ControllerSequence::BusyDelay => (ControllerSequence::IrqDelay, timings::IRQ_DELAY),
        ControllerSequence::IrqDelay => {
            psx.cdrom.command = None;
            let irq_code = psx.cdrom.controller.irq_code;

            super::set_controller_irq(psx, irq_code);

            (ControllerSequence::Idle, 0)
        }
        ControllerSequence::AsyncRxPush => {
            let b = psx.cdrom.controller.response.pop();
            psx.cdrom.host_response.push(b);

            if psx.cdrom.controller.response.is_empty() {
                // No busy flag for async transfer, we move on directly to the IRQ delay
                (ControllerSequence::IrqDelay, timings::IRQ_DELAY)
            } else {
                (ControllerSequence::AsyncRxPush, timings::RX_PUSH)
            }
        }
    };

    psx.cdrom.controller.sequence = next_step;
    psx.cdrom.controller.sequence_timer = duration;
}

/// Attempt to start a new command
pub fn maybe_start_command(psx: &mut Psx) -> bool {
    let controller = &mut psx.cdrom.controller;

    if controller.in_command() {
        // We're already running a command
        return false;
    }

    controller.start_command();
    true
}

fn execute_command(psx: &mut Psx, command: u8) {
    let (min_param, max_param, handler): (u8, u8, fn(&mut Psx)) = match command {
        0x01 => (0, 0, commands::get_stat),
        0x02 => (3, 3, commands::set_loc),
        0x03 => (0, 1, commands::play),
        // ReadN
        0x06 => (0, 0, commands::read),
        0x09 => (0, 0, commands::pause),
        0x0a => (0, 0, commands::init),
        0x0b => (0, 0, commands::mute),
        0x0c => (0, 0, commands::demute),
        0x0d => (2, 2, commands::set_filter),
        0x0e => (1, 1, commands::set_mode),
        0x0f => (0, 0, commands::get_param),
        0x11 => (0, 0, commands::get_loc_p),
        0x13 => (0, 0, commands::get_tn),
        0x14 => (1, 1, commands::get_td),
        0x15 => (0, 0, commands::seek_l),
        0x16 => (0, 0, commands::seek_p),
        0x19 => (1, 1, commands::test),
        0x1a => (0, 0, commands::get_id),
        // ReadS
        0x1b => (0, 0, commands::read),
        0x1e => (0, 0, commands::read_toc),
        c => unimplemented!("Unhandled CD command 0x{:02x}", c),
    };

    let nparams = psx.cdrom.controller.params.len();

    if nparams < min_param || nparams > max_param {
        panic!(
            "Wrong number of parameters for CD command 0x{:02x}: {}",
            command, nparams
        );
    }

    handler(psx);
}

pub fn maybe_process_async_response(psx: &mut Psx) {
    if let Some((0, handler)) = psx.cdrom.controller.get_async_response() {
        // The async response is ready, see if the controller is ready to process it
        if !psx.cdrom.controller.in_command() {
            // We can run the response sequence
            psx.cdrom.controller.clear_response();

            let rx_delay = handler(psx);

            psx.cdrom.controller.sequence = ControllerSequence::AsyncRxPush;
            psx.cdrom.controller.sequence_timer = rx_delay;
        }
    }
}

/// Description of the Controller processing sequence
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
enum ControllerSequence {
    /// Sub-CPU waits for commands and async events
    Idle,
    /// Command pending, wait for the controller to start the
    /// execution.
    CommandPending,
    /// Parameter transfer
    ParamPush,
    /// Command is executed
    Execution,
    /// Response FIFO is cleared
    RxFlush,
    /// Response transfer
    RxPush,
    /// Busy flag goes down at the end of this step
    BusyDelay,
    /// IRQ is triggered at the end of this step
    IrqDelay,
    /// Async response transfer
    AsyncRxPush,
}

/// Various IRQ codes used by the controller
#[derive(Clone, Copy, Debug)]
pub enum IrqCode {
    /// A CD sector has been read and is ready to be processed.
    SectorReady = 1,
    /// Command succesful, 2nd response.
    AsyncOk = 2,
    /// Command succesful, used for the 1st response.
    Ok = 3,
    /// Error: invalid command, disc command while do disc is present
    /// etc...
    Error = 5,
}

/// CDROM disc state
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DriveState {
    /// Motor off, nothing is happening.
    Idle,
    /// "Stand by" state, motor on
    /// Motor is running but we're not reading anything
    Paused,
    /// We're reading the table of contents
    ReadToc,
    /// We're seeking to the target sector
    Seeking(SeekType, AfterSeek),
    /// The drive is reading a sector
    Reading,
    /// The drive is playing an audio track
    Playing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum SeekType {
    /// Seek "L", used mainly to seek data tracks
    Data,
    /// Seek "P", used mainly to seek audio tracks
    Audio,
}

/// Describes what state the drive should be put in after the seek sequence has completed
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum AfterSeek {
    Pause,
    Read,
    Play,
}

mod commands {
    //! The various CD-ROM commands

    use super::disc::Region;
    use super::{timings, AfterSeek, Bcd, CycleCount, DriveState, IrqCode, Msf, Psx, SeekType};

    /// Read the drive's status byte
    pub fn get_stat(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();
    }

    /// Tell the CDROM controller where the next seek should take us (but do not physically perform
    /// the seek yet)
    pub fn set_loc(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        // Parameters are in BCD.
        let m = controller.params.pop();
        let s = controller.params.pop();
        let f = controller.params.pop();

        controller.next_seek_target = match Msf::from_bcd(m, s, f) {
            Some(m) => m,
            // XXX: what happens if invalid BCD is used?
            None => unimplemented!("Invalid MSF in set loc: {:02x}:{:02x}:{:02x}", m, s, f),
        };

        controller.seek_target_pending = true;

        controller.push_drive_status();
    }

    /// Start playing an audio track
    pub fn play(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        controller.restart_audio();

        if !controller.params.is_empty() {
            // We're given a track to seek to before we start playing
            let track = controller.params.pop();
            let track = match Bcd::from_binary(track) {
                Some(t) => t,
                None => unimplemented!("Invalid BCD: {:x}", track),
            };

            let start_msf = match controller.get_track_start(track) {
                Some(msf) => msf,
                None => unimplemented!("Track {} not found", track),
            };

            controller.next_seek_target = start_msf;

            controller.seek_target_pending = true;
            let delay = controller.do_seek(SeekType::Audio, AfterSeek::Play);
            controller.next_sector = Some(delay);
            return;
        }

        if controller.drive_state == DriveState::Playing {
            // Nothing to do
            return;
        }

        // XXX Taken from mednafen, this is different from the "read" algorithm, need to
        // double-check
        if !controller.seek_target_pending && controller.drive_state != DriveState::Playing {
            // Seek back to the actual target sector since we might have drifted away
            controller.next_seek_target = controller.seek_target;
            controller.seek_target_pending = true;
        }

        let delay = controller.do_seek(SeekType::Audio, AfterSeek::Play);
        controller.next_sector = Some(delay);
    }

    /// Start data read sequence. This is the implementation for both ReadN and ReadS, apparently
    /// the only difference between the two is that ReadN will retry in case of an error while
    /// ReadS will continue to the next sector (useful for streaming audio/movies). In our emulator
    /// for now we'll just pretend no error ever occurs. Are there any reasons to simulate errors?
    /// Maybe for some copy-protection schemes?
    pub fn read(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        if controller.drive_state == DriveState::Reading {
            warn!("CDROM READ while we're already reading");

            if !controller.seek_target_pending {
                // Nothing to do
                return;
            }
        }

        controller.restart_audio();

        // We're either starting a read, or we were already reading but we have a new target.
        if !controller.seek_target_pending {
            if controller.drive_state == DriveState::Paused {
                // Seek back to the actual target sector since we might have drifted away
                controller.next_seek_target = controller.seek_target;
            } else {
                // We stay where we are
                controller.next_seek_target = controller.position;
            }
        }

        // XXX From mednafen: a seek always take place when we start reading, even if we're at the
        // right position. Seems odd, probably worth double-checking.
        let delay = controller.do_seek(SeekType::Data, AfterSeek::Read);

        controller.next_sector = Some(delay);
    }

    /// Stop reading sectors but remain at the same position on the disc
    pub fn pause(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        // *Before* we change the state
        controller.push_drive_status();

        let state = controller.drive_state;

        let async_delay = if state == DriveState::Paused || state == DriveState::Idle {
            9000
        } else {
            controller.drive_state = DriveState::Paused;

            controller.pending_notification = None;

            // XXX Mednafen has a weird tweak to self.position here, with comment "See: Bedlam,
            // Rise 2"

            // XXX Very very rough approximation, can change based on many factors. Need to
            // come up with a more accurate heuristic
            1_000_000
        };

        controller.schedule_async_response(async_delay, async_pause);
    }

    fn async_pause(psx: &mut Psx) -> CycleCount {
        psx.cdrom.controller.push_drive_status();

        timings::PAUSE_RX_PUSH
    }

    /// Reinitialize the CD ROM controller
    pub fn init(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        // XXX Mednafen has a more complicated and probably more accurate implementation, for now I
        // keep it simple
        controller.drive_state = DriveState::Paused;
        controller.seek_target = Msf::from_bcd(0x00, 0x02, 0x00).unwrap();
        controller.position = controller.seek_target;
        controller.next_seek_target = controller.seek_target;
        controller.seek_target_pending = false;
        controller.pending_notification = None;
        controller.double_speed = false;
        controller.xa_adpcm_to_spu = false;
        controller.read_whole_sector = true;
        controller.sector_size_override = false;
        controller.filter_enabled = false;
        controller.report_interrupts = false;
        controller.autopause = false;
        controller.cdda_mode = false;
        controller.next_sector = Some(900_000);

        controller.restart_audio();

        controller.schedule_async_response(900_000, async_init);
    }

    fn async_init(psx: &mut Psx) -> CycleCount {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        timings::INIT_RX_PUSH
    }

    /// Mute CDROM audio playback
    pub fn mute(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();
    }

    /// Demute CDROM audio playback
    pub fn demute(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();
    }

    /// Filter for ADPCM sectors
    pub fn set_filter(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.filter_file = controller.params.pop();
        controller.filter_channel = controller.params.pop();

        controller.push_drive_status();
    }

    /// Configure the behaviour of the CDROM drive
    pub fn set_mode(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        let mode = controller.params.pop();

        controller.double_speed = (mode >> 7) & 1 != 0;
        controller.xa_adpcm_to_spu = (mode >> 6) & 1 != 0;
        controller.read_whole_sector = (mode >> 5) & 1 != 0;
        controller.sector_size_override = (mode >> 4) & 1 != 0;
        controller.filter_enabled = (mode >> 3) & 1 != 0;
        controller.report_interrupts = (mode >> 2) & 1 != 0;
        controller.autopause = (mode >> 1) & 1 != 0;
        controller.cdda_mode = mode & 1 != 0;

        if controller.autopause || controller.report_interrupts || controller.sector_size_override {
            unimplemented!("CDROM: unhandled mode: {:02x}", mode);
        }

        controller.push_drive_status();
    }

    /// Return various parameters of the CDROM controller
    pub fn get_param(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;
        let mut mode = 0u8;

        mode |= (controller.double_speed as u8) << 7;
        mode |= (controller.xa_adpcm_to_spu as u8) << 6;
        mode |= (controller.read_whole_sector as u8) << 5;
        mode |= (controller.sector_size_override as u8) << 4;
        mode |= (controller.filter_enabled as u8) << 3;
        mode |= (controller.report_interrupts as u8) << 2;
        mode |= (controller.autopause as u8) << 1;
        mode |= controller.cdda_mode as u8;

        let response = [
            controller.drive_status(),
            mode,
            0, // Apparently always 0
            controller.filter_file,
            controller.filter_channel,
        ];

        controller.response.push_slice(&response);
    }

    /// Get the current position of the drive head by returning the contents of the Q subchannel
    pub fn get_loc_p(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        if controller.position < Msf::from_bcd(0x00, 0x02, 0x00).unwrap() {
            // The values returned in the track 01 pregap are strange, The absolute MSF seems
            // correct but the track MSF looks like garbage.
            //
            // For instance after seeking at 00:01:25 the track MSF returned by GetLocP is 00:00:49
            // with my PAL Spyro disc.
            unimplemented!("GetLocP while in track1 pregap");
        }

        // Fixme: All this data should be extracted from the subchannel Q (when available in
        // cdimage).

        let metadata = controller.sector.metadata();

        // The position returned by get_loc_p seems to be ahead of the currently read sector
        // *sometimes*. Probably because of the way the subchannel data is buffered? Let's not
        // worry about it for now.
        //
        // In my tests on the real hardware I seem to get generally 1 sector a head, for instance
        // when I'm reading multiple sectors in a row I get the data from sector 12:02:05 when
        // get_loc_p reliably returns 01:01 12:00:06 12:02:06. get_loc_l does return 12:02:05 in
        // the same situation.
        //
        // Mednafen also seems to me using one sector of "read ahead" so I suppose it adds up.
        let abs_msf = metadata.msf;

        // Position within the current track
        let track_msf = metadata.track_msf;

        let track = metadata.track;
        let index = metadata.index;

        let (track_m, track_s, track_f) = track_msf.into_bcd();

        let (abs_m, abs_s, abs_f) = abs_msf.into_bcd();

        let response_bcd = [track, index, track_m, track_s, track_f, abs_m, abs_s, abs_f];

        for v in &response_bcd {
            controller.response.push(v.bcd());
        }
    }

    /// Return the first and last track number for the current session
    pub fn get_tn(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        let first_track;
        let last_track;

        match controller.disc {
            Some(ref mut disc) => {
                let image = disc.image();
                let toc = image.toc();
                let tracks = toc.tracks();

                if tracks.is_empty() {
                    // Disc without any tracks?
                    unimplemented!();
                }

                first_track = tracks.first().unwrap().track;
                last_track = tracks.last().unwrap().track;
            }
            None => unimplemented!(),
        }

        let response = [
            controller.drive_status(),
            first_track.bcd(),
            last_track.bcd(),
        ];

        controller.response.push_slice(&response);
    }

    /// Return the first and last track number for the current session
    pub fn get_td(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        let track = controller.params.pop();
        let track = match Bcd::from_binary(track) {
            Some(t) => t,
            None => unimplemented!("Invalid BCD: {:x}", track),
        };

        let start_msf = match controller.get_track_start(track) {
            Some(msf) => msf,
            None => {
                warn!("Get TD for unknown track {}", track);
                controller.push_drive_error();
                controller.response.push(0x10);
                controller.irq_code = IrqCode::Error;
                return;
            }
        };

        // Frame isn't returned by this command
        let (m, s, _f) = start_msf.into_bcd();

        let response = [controller.drive_status(), m.bcd(), s.bcd()];

        controller.response.push_slice(&response);
    }

    /// Execute seek. Target is given by previous "set_loc" command.
    ///
    /// This version of seek is supposed to only work on data tracks as it uses the CD-ROM sector
    /// header to aim at the final location (according to No$, and as far as I can tell mednafen
    /// implements it that way).
    pub fn seek_l(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        let seek_time = controller.do_seek(SeekType::Data, AfterSeek::Pause);

        controller.schedule_async_response(seek_time, async_seek_l);
    }

    /// Execute seek. Target is given by previous "set_loc" command.
    ///
    /// This version of seek is supposed to use the subchannel Q data to aim at the final location
    /// and should therefore work anywhere on disc, not just data tracks.
    pub fn seek_p(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        let seek_time = controller.do_seek(SeekType::Audio, AfterSeek::Pause);

        controller.schedule_async_response(seek_time, async_seek_l);
    }

    fn async_seek_l(psx: &mut Psx) -> CycleCount {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        timings::SEEK_L_RX_PUSH
    }

    /// The test command can do a whole bunch of stuff, the first parameter says what
    pub fn test(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        match controller.params.pop() {
            0x20 => test_version(psx),
            n => unimplemented!("Unhandled CD test subcommand 0x{:x}", n),
        }
    }

    fn test_version(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        // Values returned by my PAL SCPH-7502 console:
        controller.response.push(0x98); // Year
        controller.response.push(0x06); // Month
        controller.response.push(0x10); // Day
        controller.response.push(0xc3); // Version
    }

    /// Read the CD-ROM's identification string. This is how the BIOS checks that the disc is an
    /// official PlayStation disc (and not a copy) and handles region locking.
    pub fn get_id(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        if controller.disc.is_some() {
            controller.push_drive_status();

            controller.schedule_async_response(timings::GET_ID_ASYNC, async_get_id);
        } else {
            // No disc, pretend the shell is open
            controller.response.push(0x11);
            controller.response.push(0x80);

            controller.irq_code = IrqCode::Error;
        }
    }

    fn async_get_id(psx: &mut Psx) -> CycleCount {
        let controller = &mut psx.cdrom.controller;

        // If we're here we must have a disc
        let disc = controller.disc.as_ref().unwrap();

        let response = [
            // Status + bit 3 if unlicensed/audio
            controller.drive_status(),
            // Licensed, not audio, not missing
            0x00,
            // Disc type (mode2 apparently?)
            0x20,
            // Not sure what this one does. No$ says "8bit
            // ATIP from Point=C0h, if session info exists",
            // not sure what it means. Seems to be 0 for all
            // CDs I've tested...
            0x00,
            // Region string: "SCEI" for japan, "SCEE" for
            // Europe and "SCEA" for US.
            b'S',
            b'C',
            b'E',
            match disc.region() {
                Region::Japan => b'I',
                Region::NorthAmerica => b'A',
                Region::Europe => b'E',
            },
        ];

        controller.response.push_slice(&response);

        timings::GET_ID_RX_PUSH
    }

    /// Instruct the CD drive to read the table of contents
    pub fn read_toc(psx: &mut Psx) {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        controller.drive_state = DriveState::ReadToc;
        controller.next_sector = None;
        // After the TOC is read the read head is returned to its original position, no need to
        // modify `position`

        controller.schedule_async_response(timings::READ_TOC_ASYNC, async_read_toc);
    }

    fn async_read_toc(psx: &mut Psx) -> CycleCount {
        let controller = &mut psx.cdrom.controller;

        controller.push_drive_status();

        controller.drive_state = DriveState::Paused;
        controller.next_sector = Some(controller.cycles_per_sector());

        timings::READ_TOC_RX_PUSH
    }
}

/// Possible frequencies for CD audio. The values are the ratio of the frequency to the standard
/// 44.1kHz frequency in 1/7th of a sample.
#[derive(Copy, Clone, PartialEq, Eq)]
enum AudioFrequency {
    /// CD-DA (normal CD audio) at 2x drive speed, 2 * 44.1kHz
    #[allow(dead_code)]
    Da2x = 14,
    /// CD-DA (normal CD audio) at 1x drive speed. That's the usual frequency for audio tracks and
    /// the frequency the PSX's SPU works with.
    Da1x = 7,
    /// Compressed CD-ROM XA ADPCM audio sector at 37.8kHz, that is 6/7 * 44.1kHz
    Xa37k8 = 6,
    /// Compressed CD-ROM XA ADPCM audio sector at 18.9kHz, that is 3/7 * 44.1kHz
    Xa18k9 = 3,
}

mod timings {
    //! CD controller timings, expressed in CPU clock cycles.
    //!
    //! Most of those timings are rough approximations, there can be massive variations in
    //! execution time on the real hardware, probably because the controller runs its own
    //! asynchronous main loop and has to handle all the events from the physical drive at the same
    //! time it processes the host commands.

    use super::CycleCount;

    /// Range of the delay between the moment a command starts being processed and the parameter
    /// transfer sequence. This delay is *extremely* variable on the real hardware to shorter and
    /// longer delays are possible but they're uncommon.
    pub const COMMAND_PENDING: (CycleCount, CycleCount) = (9_400, 15_400);

    /// Approximate duration of a single parameter transfer
    pub const PARAM_PUSH: CycleCount = 1_800;

    /// Delay between the end of the last parameter push and the RX FIFO clear. I assume that's
    /// when the actual command execution takes place but I haven't tested it thoroughly. It
    /// shouldn't really matter anyway.
    pub const EXECUTION: CycleCount = 2_000;

    /// Delay between the RX FIFO clear and the first response byte being pushed onto the RX FIFO.
    pub const RX_FLUSH: CycleCount = 3_500;

    /// Time taken by each additional response bytes
    pub const RX_PUSH: CycleCount = 1_500;

    /// Delay between the moment the last response byte gets pushed and the moment the busy flag
    /// goes low.
    pub const BUSY_DELAY: CycleCount = 3_300;

    /// Delay between the moment the busy flag goes low and the moment the IRQ triggers OR in the
    /// case of async events the delay between the last response push and the IRQ
    pub const IRQ_DELAY: CycleCount = 2_000;

    /// Delay between GetId command execution and the asyncronous RX_CLEAR
    pub const GET_ID_ASYNC: CycleCount = 15_000;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous GetId
    /// response
    pub const GET_ID_RX_PUSH: CycleCount = 3_100;

    /// Very rough estimate of the time taken to read the table of content. On my console it takes
    /// around 1 second (and probably varies depending on the disc. I'll just use ~0.5s for the
    /// emulator, I doubt it matters much in practice.
    pub const READ_TOC_ASYNC: CycleCount = 16_000_000;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous ReadToc
    /// response
    pub const READ_TOC_RX_PUSH: CycleCount = 1_700;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous SeekL
    /// response
    pub const SEEK_L_RX_PUSH: CycleCount = 1_700;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous Read(S/N)
    /// response
    pub const READ_RX_PUSH: CycleCount = 1_800;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous Pause
    /// response
    pub const PAUSE_RX_PUSH: CycleCount = 1_700;

    /// Delay between the asynchronous RX_CLEAR and first param push for the asynchronous Init
    /// response
    pub const INIT_RX_PUSH: CycleCount = 1_700;
}
