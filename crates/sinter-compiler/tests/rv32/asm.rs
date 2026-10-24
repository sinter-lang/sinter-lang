//! A small two-pass RV32I assembler for the RV32 example cores' test programs.
//!
//! Supports every RV32I base instruction, ABI register names, labels, `#` /
//! `//` comments, `.word`, and the common pseudo-instructions (`nop`, `li`,
//! `la`, `mv`, `not`, `neg`, `j`, `jr`, `ret`, `call`, `beqz`, `bnez`, `blez`,
//! `bgez`, `bltz`, `bgtz`, `bgt`, `ble`, `bgtu`, `bleu`). Branch and jump
//! targets are labels or numeric byte offsets.

use std::collections::HashMap;

/// Assemble `src` into instruction words, starting at address 0.
pub fn assemble(src: &str) -> Result<Vec<u32>, String> {
    // Pass 1: lay out each line's address, collecting label addresses.
    let mut labels = HashMap::new();
    let mut lines = Vec::new();
    let mut pc = 0u32;
    for (n, raw) in src.lines().enumerate() {
        let line_no = n + 1;
        let mut text = strip_comment(raw).trim();
        while let Some((label, rest)) = split_label(text) {
            if labels.insert(label.to_string(), pc).is_some() {
                return Err(format!("line {line_no}: duplicate label `{label}`"));
            }
            text = rest.trim();
        }
        if text.is_empty() {
            continue;
        }
        let (mnemonic, args) = split_instruction(text);
        let size = size_of(&mnemonic, &args).map_err(|e| format!("line {line_no}: {e}"))?;
        lines.push((line_no, pc, mnemonic, args));
        pc += 4 * size;
    }

    // Pass 2: encode.
    let mut words = Vec::new();
    for (line_no, pc, mnemonic, args) in lines {
        let ctx = Ctx {
            pc,
            labels: &labels,
        };
        let encoded = encode(&ctx, &mnemonic, &args).map_err(|e| format!("line {line_no}: {e}"))?;
        words.extend(encoded);
    }
    Ok(words)
}

/// Format `words` as a `rom` hex file. The compiler zero-fills the rest of a
/// `depth`-word ROM, and a zero word is an illegal instruction, so a runaway
/// pc halts the core.
pub fn to_hex(words: &[u32], depth: usize) -> Result<String, String> {
    if words.len() > depth {
        return Err(format!(
            "program is {} words, but the ROM holds {depth}",
            words.len()
        ));
    }
    Ok(words.iter().map(|w| format!("{w:08x}\n")).collect())
}

fn strip_comment(line: &str) -> &str {
    let end = [line.find('#'), line.find("//")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(line.len());
    &line[..end]
}

/// `label: rest` → `Some(("label", "rest"))`.
fn split_label(text: &str) -> Option<(&str, &str)> {
    let (label, rest) = text.split_once(':')?;
    let label = label.trim();
    let ok = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    ok.then_some((label, rest))
}

fn split_instruction(text: &str) -> (String, Vec<String>) {
    let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let args = rest
        .split(',')
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    (mnemonic.to_ascii_lowercase(), args)
}

/// How many words `mnemonic` assembles to.
fn size_of(mnemonic: &str, args: &[String]) -> Result<u32, String> {
    Ok(match mnemonic {
        "la" => 2,
        "li" => {
            let v = parse_imm(arg(args, 1)?)?;
            if fits_signed(v, 12) {
                1
            } else {
                2
            }
        }
        _ => 1,
    })
}

struct Ctx<'a> {
    pc: u32,
    labels: &'a HashMap<String, u32>,
}

impl Ctx<'_> {
    /// A branch / jump target (label or numeric offset) as a pc-relative offset.
    fn offset(&self, target: &str) -> Result<i64, String> {
        match self.labels.get(target) {
            Some(&addr) => Ok(i64::from(addr) - i64::from(self.pc)),
            None => parse_imm(target),
        }
    }

    /// A label's absolute address, or a numeric value.
    fn value(&self, v: &str) -> Result<i64, String> {
        match self.labels.get(v) {
            Some(&addr) => Ok(i64::from(addr)),
            None => parse_imm(v),
        }
    }
}

fn arg(args: &[String], i: usize) -> Result<&str, String> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| format!("missing operand {}", i + 1))
}

fn expect_args(args: &[String], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!("expected {n} operands, found {}", args.len()))
    }
}

fn reg(name: &str) -> Result<u32, String> {
    const ABI: [&str; 32] = [
        "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
        "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
        "t5", "t6",
    ];
    let name = name.trim().to_ascii_lowercase();
    if name == "fp" {
        return Ok(8);
    }
    if let Some(i) = ABI.iter().position(|&a| a == name) {
        return Ok(i as u32);
    }
    name.strip_prefix('x')
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|&n| n < 32)
        .ok_or_else(|| format!("unknown register `{name}`"))
}

fn parse_imm(text: &str) -> Result<i64, String> {
    let t = text.trim().replace('_', "");
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, t.clone()),
    };
    let v = if let Some(h) = body.strip_prefix("0x") {
        i64::from_str_radix(h, 16)
    } else if let Some(b) = body.strip_prefix("0b") {
        i64::from_str_radix(b, 2)
    } else {
        body.parse::<i64>()
    }
    .map_err(|_| format!("invalid immediate `{text}`"))?;
    Ok(if neg { -v } else { v })
}

fn fits_signed(v: i64, bits: u32) -> bool {
    let half = 1i64 << (bits - 1);
    (-half..half).contains(&v)
}

/// `imm(rs1)` → `(imm, rs1)`.
fn mem_operand(text: &str) -> Result<(i64, u32), String> {
    let (imm, rest) = text
        .split_once('(')
        .ok_or_else(|| format!("expected `imm(reg)`, found `{text}`"))?;
    let reg_name = rest
        .strip_suffix(')')
        .ok_or_else(|| format!("expected `imm(reg)`, found `{text}`"))?;
    let imm = if imm.trim().is_empty() {
        0
    } else {
        parse_imm(imm)?
    };
    Ok((imm, reg(reg_name)?))
}

fn check_imm(v: i64, bits: u32, what: &str) -> Result<u32, String> {
    if fits_signed(v, bits) {
        Ok((v as u32) & ((1u32 << bits) - 1))
    } else {
        Err(format!("{what} {v} does not fit in {bits} signed bits"))
    }
}

fn r_type(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, op: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}

fn i_type(imm: i64, rs1: u32, f3: u32, rd: u32, op: u32) -> Result<u32, String> {
    Ok((check_imm(imm, 12, "immediate")? << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op)
}

fn s_type(imm: i64, rs2: u32, rs1: u32, f3: u32) -> Result<u32, String> {
    let i = check_imm(imm, 12, "store offset")?;
    Ok(((i >> 5) << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | ((i & 0x1F) << 7) | 0x23)
}

fn b_type(off: i64, rs2: u32, rs1: u32, f3: u32) -> Result<u32, String> {
    if off % 2 != 0 {
        return Err(format!("branch offset {off} is odd"));
    }
    let i = check_imm(off, 13, "branch offset")?;
    Ok((((i >> 12) & 1) << 31)
        | (((i >> 5) & 0x3F) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (f3 << 12)
        | (((i >> 1) & 0xF) << 8)
        | (((i >> 11) & 1) << 7)
        | 0x63)
}

fn j_type(off: i64, rd: u32) -> Result<u32, String> {
    if off % 2 != 0 {
        return Err(format!("jump offset {off} is odd"));
    }
    let i = check_imm(off, 21, "jump offset")?;
    Ok((((i >> 20) & 1) << 31)
        | (((i >> 1) & 0x3FF) << 21)
        | (((i >> 11) & 1) << 20)
        | (((i >> 12) & 0xFF) << 12)
        | (rd << 7)
        | 0x6F)
}

/// `lui` + `addi` loading the 32-bit value `v` into `rd`.
fn load_const(rd: u32, v: i64) -> Result<Vec<u32>, String> {
    let v = v as u32;
    // `addi` sign-extends its 12 bits, so round the upper part up when bit 11 is set.
    let hi = v.wrapping_add(0x800) >> 12;
    let lo = i64::from(((v & 0xFFF) as i32) << 20 >> 20);
    Ok(vec![
        (hi << 12) | (rd << 7) | 0x37,
        i_type(lo, rd, 0, rd, 0x13)?,
    ])
}

fn encode(ctx: &Ctx, m: &str, a: &[String]) -> Result<Vec<u32>, String> {
    const OP: u32 = 0x33;
    const OP_IMM: u32 = 0x13;
    let r = |i: usize| -> Result<u32, String> { reg(arg(a, i)?) };
    let alu = |f3: u32, f7: u32| -> Result<Vec<u32>, String> {
        expect_args(a, 3)?;
        Ok(vec![r_type(f7, r(2)?, r(1)?, f3, r(0)?, OP)])
    };
    let alu_imm = |f3: u32| -> Result<Vec<u32>, String> {
        expect_args(a, 3)?;
        Ok(vec![i_type(
            parse_imm(arg(a, 2)?)?,
            r(1)?,
            f3,
            r(0)?,
            OP_IMM,
        )?])
    };
    let shift_imm = |f3: u32, f7: u32| -> Result<Vec<u32>, String> {
        expect_args(a, 3)?;
        let sh = parse_imm(arg(a, 2)?)?;
        if !(0..32).contains(&sh) {
            return Err(format!("shift amount {sh} is out of range"));
        }
        Ok(vec![r_type(f7, sh as u32, r(1)?, f3, r(0)?, OP_IMM)])
    };
    let branch = |f3: u32, swap: bool| -> Result<Vec<u32>, String> {
        expect_args(a, 3)?;
        let (rs1, rs2) = if swap { (r(1)?, r(0)?) } else { (r(0)?, r(1)?) };
        Ok(vec![b_type(ctx.offset(arg(a, 2)?)?, rs2, rs1, f3)?])
    };
    let branch_zero = |f3: u32, swap: bool| -> Result<Vec<u32>, String> {
        expect_args(a, 2)?;
        let (rs1, rs2) = if swap { (0, r(0)?) } else { (r(0)?, 0) };
        Ok(vec![b_type(ctx.offset(arg(a, 1)?)?, rs2, rs1, f3)?])
    };
    Ok(match m {
        "lui" | "auipc" => {
            expect_args(a, 2)?;
            let v = parse_imm(arg(a, 1)?)?;
            if !(0..(1 << 20)).contains(&v) {
                return Err(format!("upper immediate {v} does not fit in 20 bits"));
            }
            let op = if m == "lui" { 0x37 } else { 0x17 };
            vec![((v as u32) << 12) | (r(0)? << 7) | op]
        }
        "jal" => match a.len() {
            1 => vec![j_type(ctx.offset(arg(a, 0)?)?, 1)?],
            2 => vec![j_type(ctx.offset(arg(a, 1)?)?, r(0)?)?],
            _ => return Err("expected `jal [rd,] target`".into()),
        },
        "jalr" => match a.len() {
            1 => vec![i_type(0, r(0)?, 0, 1, 0x67)?],
            2 => {
                let (imm, rs1) = mem_operand(arg(a, 1)?)?;
                vec![i_type(imm, rs1, 0, r(0)?, 0x67)?]
            }
            3 => vec![i_type(parse_imm(arg(a, 2)?)?, r(1)?, 0, r(0)?, 0x67)?],
            _ => return Err("expected `jalr rd, imm(rs1)`".into()),
        },
        "beq" => branch(0, false)?,
        "bne" => branch(1, false)?,
        "blt" => branch(4, false)?,
        "bge" => branch(5, false)?,
        "bltu" => branch(6, false)?,
        "bgeu" => branch(7, false)?,
        "bgt" => branch(4, true)?,
        "ble" => branch(5, true)?,
        "bgtu" => branch(6, true)?,
        "bleu" => branch(7, true)?,
        "beqz" => branch_zero(0, false)?,
        "bnez" => branch_zero(1, false)?,
        "bltz" => branch_zero(4, false)?,
        "bgez" => branch_zero(5, false)?,
        "bgtz" => branch_zero(4, true)?,
        "blez" => branch_zero(5, true)?,
        "lb" | "lh" | "lw" | "lbu" | "lhu" => {
            expect_args(a, 2)?;
            let f3 = match m {
                "lb" => 0,
                "lh" => 1,
                "lw" => 2,
                "lbu" => 4,
                _ => 5,
            };
            let (imm, rs1) = mem_operand(arg(a, 1)?)?;
            vec![i_type(imm, rs1, f3, r(0)?, 0x03)?]
        }
        "sb" | "sh" | "sw" => {
            expect_args(a, 2)?;
            let f3 = match m {
                "sb" => 0,
                "sh" => 1,
                _ => 2,
            };
            let (imm, rs1) = mem_operand(arg(a, 1)?)?;
            vec![s_type(imm, r(0)?, rs1, f3)?]
        }
        "addi" => alu_imm(0)?,
        "slti" => alu_imm(2)?,
        "sltiu" => alu_imm(3)?,
        "xori" => alu_imm(4)?,
        "ori" => alu_imm(6)?,
        "andi" => alu_imm(7)?,
        "slli" => shift_imm(1, 0)?,
        "srli" => shift_imm(5, 0)?,
        "srai" => shift_imm(5, 0x20)?,
        "add" => alu(0, 0)?,
        "sub" => alu(0, 0x20)?,
        "sll" => alu(1, 0)?,
        "slt" => alu(2, 0)?,
        "sltu" => alu(3, 0)?,
        "xor" => alu(4, 0)?,
        "srl" => alu(5, 0)?,
        "sra" => alu(5, 0x20)?,
        "or" => alu(6, 0)?,
        "and" => alu(7, 0)?,
        "fence" => vec![0x0FF0_000F],
        "ecall" => vec![0x0000_0073],
        "ebreak" => vec![0x0010_0073],
        ".word" => {
            expect_args(a, 1)?;
            vec![ctx.value(arg(a, 0)?)? as u32]
        }
        // Pseudo-instructions.
        "nop" => vec![0x0000_0013],
        "li" => {
            expect_args(a, 2)?;
            let v = parse_imm(arg(a, 1)?)?;
            if !(-(1i64 << 31)..(1i64 << 32)).contains(&v) {
                return Err(format!("value {v} does not fit in 32 bits"));
            }
            if fits_signed(v, 12) {
                vec![i_type(v, 0, 0, r(0)?, OP_IMM)?]
            } else {
                load_const(r(0)?, v)?
            }
        }
        "la" => {
            expect_args(a, 2)?;
            load_const(r(0)?, ctx.value(arg(a, 1)?)?)?
        }
        "mv" => {
            expect_args(a, 2)?;
            vec![i_type(0, r(1)?, 0, r(0)?, OP_IMM)?]
        }
        "not" => {
            expect_args(a, 2)?;
            vec![i_type(-1, r(1)?, 4, r(0)?, OP_IMM)?]
        }
        "neg" => {
            expect_args(a, 2)?;
            vec![r_type(0x20, r(1)?, 0, 0, r(0)?, OP)]
        }
        "j" => {
            expect_args(a, 1)?;
            vec![j_type(ctx.offset(arg(a, 0)?)?, 0)?]
        }
        "call" => {
            expect_args(a, 1)?;
            vec![j_type(ctx.offset(arg(a, 0)?)?, 1)?]
        }
        "jr" => {
            expect_args(a, 1)?;
            vec![i_type(0, r(0)?, 0, 0, 0x67)?]
        }
        "ret" => vec![i_type(0, 1, 0, 0, 0x67)?],
        other => return Err(format!("unknown instruction `{other}`")),
    })
}
