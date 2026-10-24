# A halfword access at an odd address is misaligned: the core halts with pc
# at the offending instruction (and fails its `misaligned access` assert).
# The next instruction doesn't depend on it, so only the misaligned access
# itself (not a load-use stall) may stop it from running.
        li   t6, 0x80000000
        li   a0, 9
        sw   a0, 0(t6)
        lh   a1, 1(x0)
        sw   a0, 0(t6)           # never reached
        sw   a0, 0(t6)           # never reached
        ecall
