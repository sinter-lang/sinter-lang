//! STOR1 memory placement hints (DESIGN §9.15): `#[storage(regs | lutram |
//! bram)]` on a `mem` / `rom` / `fifo` field reaches the IR and the Verilog
//! array's `ram_style`, is rejected where it means nothing, and — with Yosys
//! present — maps to the ECP5 primitive it names. The sim↔Verilog oracle is
//! in `conformance.rs` (the `fn_storage` example).

#[path = "support/oracle.rs"]
mod oracle;

use std::process::Command;

use sinter_compiler::ir::Storage;
use sinter_compiler::{compile_str, verilog};

const STORAGE: &str = include_str!("../../../examples/fn_storage.snt");

fn compile_err(src: &str, needle: &str) {
    match compile_str(src) {
        Ok(_) => panic!("expected an error containing {needle:?}, but it compiled"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(needle),
                "error did not contain {needle:?}.\n  got: {msg}"
            );
        }
    }
}

/// A one-memory design whose field is `<attr> m: <ty> = 0`.
fn one_mem(attr: &str, ty: &str) -> String {
    format!(
        "struct M {{ {attr} m: {ty} = 0, }}
         impl M {{
             #[clock(domain = sys)]
             fn tick(self, a: u4, we: u1, d: u8) -> (M, u8) {{
                 let v = self.m.value();
                 (M {{ m: self.m.next(a, we, a, d) }}, v)
             }}
         }}"
    )
}

#[test]
fn each_hint_reaches_the_ir() {
    let design = compile_str(STORAGE).expect("compile");
    let hints: Vec<_> = design.modules[0]
        .mems
        .iter()
        .map(|m| (m.name.as_str(), m.storage))
        .collect();
    assert_eq!(
        hints,
        [
            ("temps", Storage::Lutram),
            ("lut", Storage::Bram),
            ("scratch", Storage::Regs)
        ]
    );
}

#[test]
fn each_hint_becomes_a_ram_style() {
    let design = compile_str(STORAGE).expect("compile");
    let sv = verilog::emit_module(&design.modules[0]);
    for line in [
        "(* ram_style = \"distributed\" *) logic [35:0] temps [0:31];",
        "(* ram_style = \"block\" *) logic [17:0] lut [0:1023];",
        "(* ram_style = \"logic\" *) logic [7:0] scratch [0:15];",
    ] {
        assert!(sv.contains(line), "missing `{line}`:\n{sv}");
    }
}

#[test]
fn no_hint_leaves_the_array_unannotated() {
    let design = compile_str(&one_mem("", "mem<u8, 16>")).expect("compile");
    assert_eq!(design.modules[0].mems[0].storage, Storage::Auto);
    let sv = verilog::emit_module(&design.modules[0]);
    assert!(!sv.contains("ram_style"), "{sv}");
}

#[test]
fn a_rom_takes_a_hint() {
    let src = "struct M {
                   #[storage(lutram)]
                   t: rom<u8, 4> = [1, 2, 3, 4],
               }
               impl M {
                   #[clock(domain = sys)]
                   fn tick(self, a: u2) -> (M, u8) {
                       let v = self.t.value();
                       (M { t: self.t.next(a) }, v)
                   }
               }";
    let design = compile_str(src).expect("compile");
    assert_eq!(design.modules[0].mems[0].storage, Storage::Lutram);
}

#[test]
fn a_hint_inside_a_nested_struct_is_kept() {
    let src = "struct Bank { #[storage(bram)] m: mem<u8, 16> = 0, }
               struct Top { bank: Bank, }
               impl Top {
                   #[clock(domain = sys)]
                   fn tick(self, a: u4, we: u1, d: u8) -> (Top, u8) {
                       let v = self.bank.m.value();
                       (Top { bank: Bank { m: self.bank.m.next(a, we, a, d) } }, v)
                   }
               }";
    let design = compile_str(src).expect("compile");
    let m = &design.modules[0].mems[0];
    assert_eq!((m.name.as_str(), m.storage), ("bank_m", Storage::Bram));
}

#[test]
fn a_lutram_fifo_puts_its_ram_in_lut_ram() {
    let src = "struct Q { #[storage(lutram)] q: fifo<u8, 16>, }
               impl Q {
                   #[clock(domain = sys)]
                   fn tick(self, push: Option<u8>, pop: u1) -> (Q, Option<u8>) {
                       let p = self.q.sender().offer(push);
                       (Q { q: self.q.next(p, pop) }, self.q.front())
                   }
               }";
    let design = compile_str(src).expect("compile");
    let m = &design.modules[0].mems[0];
    assert_eq!((m.name.as_str(), m.storage), ("q_ram", Storage::Lutram));
}

#[test]
fn a_lutram_memory_too_deep_is_rejected() {
    compile_err(
        &one_mem("#[storage(lutram)]", "mem<u8, 512>"),
        "`m` is 512 words deep, too deep for `#[storage(lutram)]` (at most 256); use `bram`",
    );
}

#[test]
fn a_hint_on_a_register_is_rejected() {
    compile_err(
        "struct M { #[storage(bram)] r: u8 = 0, }
         impl M { #[clock(domain = sys)] fn tick(self) -> (M, u8) { (M { r: self.r }, self.r) } }",
        "`#[storage(..)]` applies only to a `fifo`, `mem`, or `rom` field",
    );
}

#[test]
fn an_unknown_hint_is_rejected() {
    compile_err(&one_mem("#[storage(flash)]", "mem<u8, 16>"), "storage");
}

/// With Yosys present, `synth_ecp5` maps each hint to the primitive it names:
/// the 32 × 36 four-read-port bank to 72 `TRELLIS_DPR16X4` (2 deep × 9 wide ×
/// 4 ports), the 1024 × 18 table to one `DP16KD`, and the scratch memory to
/// flip-flops only.
#[test]
fn hints_map_to_ecp5_primitives() {
    if !oracle::tools().yosys {
        return;
    }
    let design = compile_str(STORAGE).expect("compile");
    let dir = oracle::workdir("storage-ecp5");
    let sv = dir.join("Banks.sv");
    std::fs::write(&sv, verilog::emit_module(&design.modules[0])).expect("write sv");
    let out = Command::new("yosys")
        .arg("-q")
        .arg("-p")
        .arg(format!(
            "read_verilog -sv {}; synth_ecp5 -top Banks; tee -o {} stat",
            sv.display(),
            dir.join("stat.txt").display()
        ))
        .output()
        .expect("spawn yosys");
    assert!(
        out.status.success(),
        "yosys failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stat = std::fs::read_to_string(dir.join("stat.txt")).expect("read stat");
    let count = |cell: &str| -> usize {
        stat.lines()
            .find_map(|l| {
                let mut w = l.split_whitespace();
                let n = w.next()?.parse().ok()?;
                (w.next()? == cell).then_some(n)
            })
            .unwrap_or(0)
    };
    assert_eq!(count("TRELLIS_DPR16X4"), 72, "{stat}");
    assert_eq!(count("DP16KD"), 1, "{stat}");
}
