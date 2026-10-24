# An illegal instruction halts the core with pc at the offending instruction.
        li   t6, 0x80000000
        li   a0, 7
        sw   a0, 0(t6)
        .word 0x00000000         # all zeros: not an RV32I instruction
        sw   a0, 0(t6)           # never reached
        ecall
