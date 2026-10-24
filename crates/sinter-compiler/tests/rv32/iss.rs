//! A reference RV32I instruction-set simulator for checking the RV32 example
//! cores. Written from the RV32I specification, independently of the Sinter
//! cores, and with the same memory map:
//!
//! - instruction ROM: 256 words, read at `pc[9:2]`;
//! - data RAM: 256 words at `addr[9:2]`, byte-addressed within a word by
//!   `addr[1:0]` (all of RV32I's loads and stores);
//! - output port: a store of any size to an address with bit 31 set appends
//!   the whole source register to [`Iss::port`];
//! - ECALL, EBREAK, illegal instructions, and misaligned halfword / word
//!   accesses halt with `pc` left at the halting instruction.

/// Words in the instruction ROM and in the data RAM.
pub const WORDS: usize = 256;

pub struct Iss {
    pub pc: u32,
    pub regs: [u32; 32],
    pub ram: [u32; WORDS],
    /// Every word written to the output port, in order.
    pub port: Vec<u32>,
    pub halted: bool,
    rom: Vec<u32>,
}

impl Iss {
    pub fn new(program: &[u32]) -> Self {
        let mut rom = program.to_vec();
        rom.resize(WORDS, 0);
        Iss {
            pc: 0,
            regs: [0; 32],
            ram: [0; WORDS],
            port: Vec::new(),
            halted: false,
            rom,
        }
    }

    /// Run until halted, or at most `limit` instructions. Returns how many
    /// instructions retired (the halting one included).
    pub fn run(&mut self, limit: usize) -> usize {
        let mut n = 0;
        while !self.halted && n < limit {
            self.step();
            n += 1;
        }
        n
    }

    /// Execute one instruction.
    pub fn step(&mut self) {
        if self.halted {
            return;
        }
        let inst = self.rom[((self.pc >> 2) as usize) % WORDS];
        let opcode = inst & 0x7F;
        let rd = ((inst >> 7) & 0x1F) as usize;
        let f3 = (inst >> 12) & 7;
        let rs1 = self.regs[((inst >> 15) & 0x1F) as usize];
        let rs2 = self.regs[((inst >> 20) & 0x1F) as usize];
        let f7 = inst >> 25;
        let imm_i = (inst as i32 >> 20) as u32;
        let imm_s = (((inst as i32) >> 25) << 5) as u32 | ((inst >> 7) & 0x1F);
        let imm_b = ((((inst as i32) >> 31) << 12) as u32)
            | (((inst >> 7) & 1) << 11)
            | (((inst >> 25) & 0x3F) << 5)
            | (((inst >> 8) & 0xF) << 1);
        let imm_u = inst & 0xFFFF_F000;
        let imm_j = ((((inst as i32) >> 31) << 20) as u32)
            | (inst & 0x000F_F000)
            | (((inst >> 20) & 1) << 11)
            | (((inst >> 21) & 0x3FF) << 1);
        let pc4 = self.pc.wrapping_add(4);
        let mut next = pc4;
        let write = |regs: &mut [u32; 32], v: u32| {
            if rd != 0 {
                regs[rd] = v;
            }
        };

        match opcode {
            0x37 => write(&mut self.regs, imm_u),
            0x17 => write(&mut self.regs, self.pc.wrapping_add(imm_u)),
            0x6F => {
                write(&mut self.regs, pc4);
                next = self.pc.wrapping_add(imm_j);
            }
            0x67 if f3 == 0 => {
                next = rs1.wrapping_add(imm_i) & !1;
                write(&mut self.regs, pc4);
            }
            0x63 if f3 != 2 && f3 != 3 => {
                let taken = match f3 {
                    0 => rs1 == rs2,
                    1 => rs1 != rs2,
                    4 => (rs1 as i32) < (rs2 as i32),
                    5 => (rs1 as i32) >= (rs2 as i32),
                    6 => rs1 < rs2,
                    _ => rs1 >= rs2,
                };
                if taken {
                    next = self.pc.wrapping_add(imm_b);
                }
            }
            0x03 if matches!(f3, 0 | 1 | 2 | 4 | 5) => {
                let addr = rs1.wrapping_add(imm_i);
                if misaligned(f3 & 3, addr) {
                    self.halted = true;
                    return;
                }
                let word = self.ram[((addr >> 2) as usize) % WORDS];
                let v = word >> (8 * (addr & 3));
                let v = match f3 {
                    0 => v as u8 as i8 as i32 as u32,
                    1 => v as u16 as i16 as i32 as u32,
                    4 => v & 0xFF,
                    5 => v & 0xFFFF,
                    _ => v,
                };
                write(&mut self.regs, v);
            }
            0x23 if f3 <= 2 => {
                let addr = rs1.wrapping_add(imm_s);
                if misaligned(f3, addr) {
                    self.halted = true;
                    return;
                }
                if addr >> 31 == 1 {
                    self.port.push(rs2);
                } else {
                    let cell = &mut self.ram[((addr >> 2) as usize) % WORDS];
                    let shift = 8 * (addr & 3);
                    let mask: u32 = match f3 {
                        0 => 0xFF,
                        1 => 0xFFFF,
                        _ => 0xFFFF_FFFF,
                    } << shift;
                    *cell = (*cell & !mask) | ((rs2 << shift) & mask);
                }
            }
            0x13 if f3 != 1 && f3 != 5 => write(&mut self.regs, alu(f3, false, rs1, imm_i)),
            0x13 if f3 == 1 && f7 == 0 => write(&mut self.regs, alu(f3, false, rs1, imm_i)),
            0x13 if f3 == 5 && (f7 == 0 || f7 == 0x20) => {
                write(&mut self.regs, alu(f3, f7 == 0x20, rs1, imm_i))
            }
            0x33 if f7 == 0 || (f7 == 0x20 && (f3 == 0 || f3 == 5)) => {
                write(&mut self.regs, alu(f3, f7 == 0x20, rs1, rs2))
            }
            0x0F => {}
            // ECALL, EBREAK, and anything illegal: halt at this instruction.
            _ => {
                self.halted = true;
                return;
            }
        }
        self.pc = next;
    }
}

/// Whether an access of size `size` (0 byte, 1 halfword, 2 word) at `addr`
/// is misaligned.
fn misaligned(size: u32, addr: u32) -> bool {
    match size {
        1 => addr & 1 != 0,
        2 => addr & 3 != 0,
        _ => false,
    }
}

fn alu(f3: u32, alt: bool, a: u32, b: u32) -> u32 {
    let sh = b & 31;
    match f3 {
        0 if alt => a.wrapping_sub(b),
        0 => a.wrapping_add(b),
        1 => a << sh,
        2 => u32::from((a as i32) < (b as i32)),
        3 => u32::from(a < b),
        4 => a ^ b,
        5 if alt => ((a as i32) >> sh) as u32,
        5 => a >> sh,
        6 => a | b,
        _ => a & b,
    }
}
