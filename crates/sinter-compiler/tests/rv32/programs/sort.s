# Bubble sort eight words in RAM, then write them to the output port.
        li   t6, 0x80000000
        li   s0, 0x40            # array base
        li   s1, 8               # length
        la   t0, data
        # Copy the table below into RAM, building each value with li.
        li   a0, 37
        sw   a0, 0(s0)
        li   a0, -5
        sw   a0, 4(s0)
        li   a0, 1000
        sw   a0, 8(s0)
        li   a0, 0
        sw   a0, 12(s0)
        li   a0, 0x7FFFFFFF
        sw   a0, 16(s0)
        li   a0, -2048
        sw   a0, 20(s0)
        li   a0, 37
        sw   a0, 24(s0)
        li   a0, 2
        sw   a0, 28(s0)

        addi s2, s1, -1          # outer passes
outer:  beqz s2, done
        mv   t1, s0
        mv   t2, s2
inner:  lw   a0, 0(t1)
        lw   a1, 4(t1)
        ble  a0, a1, noswap      # signed comparison
        sw   a1, 0(t1)
        sw   a0, 4(t1)
noswap: addi t1, t1, 4
        addi t2, t2, -1
        bnez t2, inner
        addi s2, s2, -1
        j    outer

done:   mv   t1, s0
        mv   t2, s1
print:  lw   a0, 0(t1)
        sw   a0, 0(t6)
        addi t1, t1, 4
        addi t2, t2, -1
        bnez t2, print
        ecall
data:   .word 0
