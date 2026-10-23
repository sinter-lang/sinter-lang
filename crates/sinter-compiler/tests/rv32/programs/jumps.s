# JAL / JALR / AUIPC: link values, computed targets, and a call / return.
        li   t6, 0x80000000
        auipc a0, 0              # a0 = pc of this instruction (8)
        sw   a0, 0(t6)
        auipc a0, 1              # pc + 0x1000
        sw   a0, 0(t6)
        jal  ra, func            # ra = address of the next instruction
        sw   ra, 0(t6)
        la   t0, target
        jalr t1, 0(t0)           # t1 = link
        sw   x0, 0(t6)           # skipped
target: sw   t1, 0(t6)
        la   t0, target2
        jalr x0, 1(t0)           # bit 0 of the target is cleared
        sw   x0, 0(t6)           # skipped
target2:
        call twice
        sw   a1, 0(t6)
        ecall

func:   li   a1, 42
        sw   a1, 0(t6)
        ret

twice:  add  a1, a1, a1
        ret
