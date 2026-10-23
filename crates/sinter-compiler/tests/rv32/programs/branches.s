# All six branch conditions, taken and not taken, at signed / unsigned edges.
# Each test writes 1 (taken) or 0 (not taken) to the output port.
        li   t6, 0x80000000
        li   s1, 0x80000000      # INT_MIN
        li   s2, -1
        li   s3, 5
        li   s4, 5

        beq  s3, s4, take_beq
        sw   x0, 0(t6)
        j    after_beq
take_beq:
        li   a0, 1
        sw   a0, 0(t6)
after_beq:
        bne  s3, s4, take_bne
        sw   x0, 0(t6)
        j    after_bne
take_bne:
        li   a0, 1
        sw   a0, 0(t6)
after_bne:
        blt  s1, s3, take_blt    # signed: INT_MIN < 5
        sw   x0, 0(t6)
        j    after_blt
take_blt:
        li   a0, 1
        sw   a0, 0(t6)
after_blt:
        bltu s1, s3, take_bltu   # unsigned: 0x80000000 > 5
        sw   x0, 0(t6)
        j    after_bltu
take_bltu:
        li   a0, 1
        sw   a0, 0(t6)
after_bltu:
        bge  s2, s1, take_bge    # -1 >= INT_MIN
        sw   x0, 0(t6)
        j    after_bge
take_bge:
        li   a0, 1
        sw   a0, 0(t6)
after_bge:
        bgeu s3, s2, take_bgeu   # 5 >= 0xFFFFFFFF is false
        sw   x0, 0(t6)
        j    after_bgeu
take_bgeu:
        li   a0, 1
        sw   a0, 0(t6)
after_bgeu:
        # A backward branch: count down from 5.
        mv   a0, s3
loop:   sw   a0, 0(t6)
        addi a0, a0, -1
        bnez a0, loop
        ecall
