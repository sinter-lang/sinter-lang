# Byte and halfword loads and stores: every lane, sign and zero extension,
# a store immediately followed by a load of the same bytes, and a load
# immediately used.
        li   t6, 0x80000000
        li   s0, 0x40
        li   a0, 0x11223344
        sw   a0, 0(s0)
        li   a1, 0xAB
        sb   a1, 1(s0)           # 0x1122AB44
        lw   a2, 0(s0)
        sw   a2, 0(t6)
        li   a1, 0xCDEF
        sh   a1, 2(s0)           # 0xCDEFAB44
        lw   a2, 0(s0)
        sw   a2, 0(t6)
        lb   a3, 1(s0)           # 0xFFFFFFAB
        sw   a3, 0(t6)
        lbu  a3, 1(s0)           # 0x000000AB
        sw   a3, 0(t6)
        lh   a3, 2(s0)           # 0xFFFFCDEF
        sw   a3, 0(t6)
        lhu  a3, 2(s0)           # 0x0000CDEF
        sw   a3, 0(t6)
        lh   a3, 0(s0)           # 0xFFFFAB44
        sw   a3, 0(t6)
        lb   a3, 0(s0)           # 0x00000044
        sw   a3, 0(t6)
        lb   a3, 3(s0)           # 0xFFFFFFCD
        sw   a3, 0(t6)

        # Build a word from four byte stores, high lane first.
        li   s1, 0x44
        li   a0, 0x01
        sb   a0, 3(s1)
        li   a0, 0x82
        sb   a0, 2(s1)
        li   a0, 0x03
        sb   a0, 1(s1)
        li   a0, 0xF4
        sb   a0, 0(s1)
        lw   a2, 0(s1)           # 0x018203F4
        sw   a2, 0(t6)

        # Store then load the same byte; load then use at once.
        li   a0, 0x5A
        sb   a0, 6(s1)
        lbu  a1, 6(s1)
        addi a1, a1, 1           # load-use
        sw   a1, 0(t6)
        lhu  a2, 6(s1)
        add  a3, a2, a2
        sw   a3, 0(t6)

        # A byte store to the port sends the whole register.
        li   a0, 0x12345678
        sb   a0, 0(t6)
        ecall
