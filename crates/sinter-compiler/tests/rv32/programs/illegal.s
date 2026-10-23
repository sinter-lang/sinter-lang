# The cores support word loads and stores only: a byte load is illegal and
# halts the core with pc at the offending instruction.
        li   t6, 0x80000000
        li   a0, 7
        sw   a0, 0(t6)
        .word 0x00000503         # lb a0, 0(x0)
        sw   a0, 0(t6)           # never reached
        ecall
