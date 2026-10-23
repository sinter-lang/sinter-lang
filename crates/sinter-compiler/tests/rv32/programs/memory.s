# LW / SW: stores then loads, back-to-back loads, load-use, address wrap
# (the RAM decodes addr[9:2]), and negative offsets.
        li   t6, 0x80000000
        li   s0, 0x100
        li   a0, 11
        sw   a0, 0(s0)
        li   a0, 22
        sw   a0, 4(s0)
        li   a0, 33
        sw   a0, -4(s0)
        lw   a1, 0(s0)
        lw   a2, 4(s0)           # back-to-back loads
        add  a3, a1, a2          # uses both loaded values at once
        sw   a3, 0(t6)
        lw   a4, -4(s0)
        sw   a4, 0(t6)
        li   t0, 0x400           # 0x400 + 0x100 wraps to word 0x40
        lw   a5, 0x100(t0)
        sw   a5, 0(t6)
        sw   a3, 0(t0)           # word 0 via the wrapped address
        lw   a6, 0(x0)
        sw   a6, 0(t6)
        lw   a7, 0(t6)           # loads ignore bit 31: RAM word 0
        sw   a7, 0(t6)
        lw   a0, 0(s0)           # load then store the loaded value
        sw   a0, 8(s0)
        lw   a1, 8(s0)
        sw   a1, 0(t6)
        ecall
