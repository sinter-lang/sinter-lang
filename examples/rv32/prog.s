# The rv32 cores' default program: write the first 12 Fibonacci numbers to the
# output port, keeping them in RAM, then sum them back from RAM and halt.
#
# Assembled to prog.hex by the test suite's assembler:
#   SINTER_BLESS=1 cargo test -p sinter-compiler --test rv32
        li   t6, 0x80000000      # output port
        li   s0, 0x80            # RAM buffer
        li   s1, 12              # count
        li   a0, 0               # fib(n)
        li   a1, 1               # fib(n + 1)
        mv   t0, s0
        mv   t1, s1
fib:    sw   a0, 0(t6)
        sw   a0, 0(t0)
        add  a2, a0, a1
        mv   a0, a1
        mv   a1, a2
        addi t0, t0, 4
        addi t1, t1, -1
        bnez t1, fib

        li   a3, 0               # sum the buffer back from RAM
        mv   t0, s0
        mv   t1, s1
sum:    lw   a2, 0(t0)
        add  a3, a3, a2
        addi t0, t0, 4
        addi t1, t1, -1
        bnez t1, sum
        sw   a3, 0(t6)
        ecall
