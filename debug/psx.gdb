# Script to ease the debugging of Rustation with GDB
#
# Run with:
#    mipsel-unknown-elf-gdb -x psx.gdb

define rsx
    set output-radix 16
    set step-mode on
    target remote localhost:9001
end
document rsx
    Connect to a local Rustation instance
end

define sd
    stepi
    disassemble/r $pc-40,+80
end
document sd
    Steps a single instruction and disassemble around PC
end

define dc
    disassemble/r $pc-40,+80
end
document dc
    Disassemble instructions around PC
end

define epc
    x/xw (0xbad00000 + 14 * 4)
end
document epc
    Dump the value of the Exception PC (Cop0 R14)
end

define depc
    disassemble/r *(0xbad00000 + 14 * 4)-40,+80
end
document depc
    Disassemble code around the Exception PC
end

define dra
    disassemble/r $ra-40,+80
end
document dra
    Disassemble code around $ra
end
