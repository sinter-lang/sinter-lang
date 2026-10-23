# Every RV32I ALU operation, register and immediate forms, at edge values.
# Each result goes to the output port (any address with bit 31 set).
        li   t6, 0x80000000      # output port
        li   s0, 0x7FFFFFFF      # INT_MAX
        li   s1, 0x80000000      # INT_MIN
        li   s2, -1
        li   s3, 5
        li   s4, 0xDEADBEEF
        add  a0, s0, s3          # signed overflow wraps
        sw   a0, 0(t6)
        sub  a0, s1, s3
        sw   a0, 0(t6)
        sub  a0, x0, s2
        sw   a0, 0(t6)
        and  a0, s4, s0
        sw   a0, 0(t6)
        or   a0, s4, s3
        sw   a0, 0(t6)
        xor  a0, s4, s2
        sw   a0, 0(t6)
        slt  a0, s1, s3          # INT_MIN < 5 (signed)
        sw   a0, 0(t6)
        slt  a0, s3, s1
        sw   a0, 0(t6)
        sltu a0, s1, s3          # 0x80000000 > 5 (unsigned)
        sw   a0, 0(t6)
        sltu a0, s3, s2
        sw   a0, 0(t6)
        sll  a0, s4, s3
        sw   a0, 0(t6)
        li   t0, 36              # shift amounts use the low 5 bits: 36 -> 4
        sll  a0, s4, t0
        sw   a0, 0(t6)
        srl  a0, s1, s3
        sw   a0, 0(t6)
        sra  a0, s1, s3          # sign fill
        sw   a0, 0(t6)
        sra  a0, s4, t0
        sw   a0, 0(t6)
        addi a0, s0, 1
        sw   a0, 0(t6)
        addi a0, s3, -2048
        sw   a0, 0(t6)
        slti a0, s2, 0
        sw   a0, 0(t6)
        sltiu a0, s3, -1         # -1 sign-extends to 0xFFFFFFFF
        sw   a0, 0(t6)
        xori a0, s4, -1          # = not
        sw   a0, 0(t6)
        ori  a0, s3, 0x7F0
        sw   a0, 0(t6)
        andi a0, s4, -16
        sw   a0, 0(t6)
        slli a0, s3, 31
        sw   a0, 0(t6)
        srli a0, s2, 1
        sw   a0, 0(t6)
        srai a0, s1, 31
        sw   a0, 0(t6)
        lui  a0, 0xFFFFF
        sw   a0, 0(t6)
        add  x0, s3, s3          # x0 stays zero
        sw   x0, 0(t6)
        ebreak
