# Rustation PlayStation Emulator

Rustation is a PlayStation emulator written entirely in Rust. This is not a
standalone emulator, it's an emulator core that implements the libretro
interface and can be used to play PlayStation game in a libretro frontend such
as RetroArch.

![Crash Bandicoot on Rustation](https://svkt.org/~simias/crash-bandicoot.png)

A lot of this code is based on
[No$ PSX specifications](http://problemkaputt.de/psx-spx.htm) and on
[Mednafen](https://mednafen.github.io/)'s PlayStation code as well as tests on
the real hardware. The codebase aims to be readable and document what's known to
be accurate, what's known to be inaccurate and what hasn't been tested yet.

# Features

The emulator is still at a fairly early phase of development and still very
glitchy and incomplete. Some notable features of this project are:

* Entirely software renderer that runs in a separate thread for better
  performance.

* Native support for 24bit rendering in software.

* Low-level CD controller emulation.

* Multithreaded CD cache implementation to avoid performance problems caused by
  high-latency storage.

* GDB-based debugger interface.

* Readable and well documented code, hopefully.

## Build

You need to [install a Rust build environment](https://www.rust-lang.org/).
Normally it's fairly straightforward, just follow the instructions on Rust's
website.

Once that's done you can build an optimized build with:

    cargo build --release

That should download all dependencies and output the core library in
`target/release/`. On Linux that file is named `librustation_ng_retro.so` but
the name may vary on other platforms.

## Run

Once the core is compiled you can load it in your favourite libretro player to
play your PlayStation games in BIN/CUE format. For instance using retroarch you
can do:

    retroarch -L librustation_ng_retro.so my_game.cue

ZIP archives containing bin/cue are also supported. If the archive contains
several cue files only the first one (in archive order) will be used.

## Troubleshooting

### Logs

If Rustation crashes, activate logging in your frontend to see what went wrong.
Rustation tends to be fairly verbose so it should tell you explicitly what
caused the failure.

To enable logging in RetroArch go to "Settings" -> "Logging". Set "Core Logging
Level" to "0 (Debug)" to get all the messages.

### Game formats

For now only BIN/CUE is supported. I'd like to support CHD eventually but for
the time being I prefer to focus on the core of the emulator. Contributions on
this front are more than welcome!

### BIOS

As with most PlayStation emulators you'll need to have a proper PlayStation BIOS
for the emulator to boot up correctly. Rustation will look for those BIOS files
in the libretro "system directory" that you should be able to configure in your
frontend.

As always look at Rustation's log messages to see whether it manages to find and
load your BIOS. It should tell you which where it looks for the files, what he
found and why it accepted or rejected any files. For instance when successfully
loading a North American game I get the following output:

    [libretro INFO] Loading "games/Crash Bandicoot (USA).cue"
    [libretro INFO] Disc serial number: SCUS-94900
    [libretro INFO] Detected disc region: NorthAmerica

This tells you that the game image has been successfully loaded and its serial
number and region have been detected.

    [INFO] [Environ]: SYSTEM_DIRECTORY: "system".
    [libretro INFO] Looking for a suitable BIOS in "system"

That tells you in which directory we are looking for the BIOS file.

    [libretro DEBUG] Ignoring "system/dmg_boot.bin": bad size
    [libretro DEBUG] Ignoring "system/gbc_bios.bin": bad size
    [libretro INFO] Found BIOS DB entry for "system/scph5500.bin": Japan/v3.0
    [libretro WARN] Ignoring "system/scph5500.bin": BadBios("\"/home/lionel/sync/roms/system/scph5500.bin\": rejected by predicate")
    [libretro WARN] Ignoring "system/saturn_bios.bin": UnknownBios
    [libretro INFO] Found BIOS DB entry for "system/scph5501.bin": NorthAmerica/v3.0
    [libretro INFO] Using BIOS "system/scph5501.bin" (NorthAmerica/v3.0)

Here you see the emulator iterating on every file in the directory, attempting
to find a suitable BIOS. If the file doesn't have the right size, isn't found in
the internal BIOS database or isn't the right region for the game, it's
rejected.

### CD Controller firmware

Unlike most PSX emulators, Rustation-ng emulates the low-level details of the CD
controller and needs an original firmware to run which is distinct from the
usual BIOS files. More details on these firmwares (and how to dump them) can be
found [on this psxdev.net forum thread](http://www.psxdev.net/forum/viewtopic.php?t=557).

The emulator has been written and tested against the SCPH-5502 firmware
(scph-5502_SC430939.bin) and is currently the only one supported. It's a PAL
(European) console firmware but the emulator hotpatches it to support other
regions as needed.

This file must be stored in the system directory alongside the BIOS. Its name
doesn't matter, the emulator will select it by size and SHA256 (similar to the
BIOS selection process described above).|

| | scph-5502_SC430939.bin |
| ------ | ------ |
| Size    | `16,896B`                                                           |
| SHA-256 | `bf590fbf6055f428138510b26a2f2006b7eab54ead48c1ddb1a1a5d2699242db` |
| SHA-1   | `f23458d13a518616a8592b8ddd668c052bc9be5a`                         |
| MD5     | `3a3b384c1c006e0cf4644a0daa202237`                                 |
