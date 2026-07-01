//! ROM1 tests (DESIGN §6.6/§6.9/§9.7): a read-only `rom<T, N>` with compile-time
//! contents (inline array literal), the read-only `value()`/`next(raddr)`
//! surface, the `Static(1)` read latency, and the ROM1 rejections. The
//! sim↔Verilog oracle is in `conformance.rs` (the `fn_rom` example).

use std::collections::HashMap;

use sinter_compiler::compile_str;
use sinter_compiler::sim::Simulator;

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

#[test]
fn rom_lowers_to_a_read_only_array_with_its_contents() {
    let design = compile_str(include_str!("../../../examples/fn_rom.snt")).expect("compile");
    let m = &design.modules[0];
    assert_eq!(m.mems.len(), 1, "one memory");
    let mem = &m.mems[0];
    assert_eq!(mem.name, "lut");
    assert_eq!(mem.depth, 8);
    // The contents are the compile-time initializer, not zeros.
    assert_eq!(mem.reset, vec![0, 1, 4, 9, 16, 25, 36, 49]);
    // Only the read-data register; a read port but NO write port (read-only).
    assert_eq!(m.regs.len(), 1);
    assert_eq!(m.regs[0].name, "lut_rdata");
    assert_eq!(m.mem_reads.len(), 1);
    assert!(m.mem_writes.is_empty(), "a rom has no write port");
}

#[test]
fn rom_read_has_static1_latency() {
    let design = compile_str(include_str!("../../../examples/fn_rom.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let step = |sim: &mut Simulator, addr: u64| {
        sim.step(
            &[("addr".to_string(), addr)]
                .into_iter()
                .collect::<HashMap<_, _>>(),
        )["out"]
    };
    // squares = [0, 1, 4, 9, 16, 25, 36, 49]; a read at cycle k surfaces at k+1.
    assert_eq!(step(&mut sim, 3), 0, "filling: read-data still 0");
    assert_eq!(step(&mut sim, 5), 9, "squares[3]");
    assert_eq!(step(&mut sim, 7), 25, "squares[5]");
    assert_eq!(step(&mut sim, 0), 49, "squares[7]");
}

// --- ROM1 rejections -------------------------------------------------------

#[test]
fn rom_without_contents_is_rejected() {
    compile_err("struct S { r: rom<u8, 4> }", "needs initial contents");
}

#[test]
fn rom_with_wrong_word_count_is_rejected() {
    compile_err("struct S { r: rom<u8, 4> = [1, 2, 3] }", "needs 4 words");
}

#[test]
fn rom_word_out_of_range_is_rejected() {
    compile_err("struct S { r: rom<u4, 2> = [1, 99] }", "does not fit");
}

#[test]
fn rom_with_non_array_contents_is_rejected() {
    compile_err("struct S { r: rom<u8, 4> = 0 }", "must be an array literal");
}

#[test]
fn writing_to_a_rom_is_rejected() {
    // A `rom`'s `next` takes only the read address — the 4-arg (write) form is a
    // `mem` operation.
    compile_err(
        "struct R { r: rom<u8, 4> = [0, 0, 0, 0] } \
         impl R { #[clock(domain = sys)] fn tick(self, a: u2, v: u8) -> (R, u8) { \
            let x = self.r.value(); (R { r: self.r.next(a, 1, a, v) }, x) } }",
        "expects 1 argument",
    );
}

#[test]
fn mem_with_array_contents_is_rejected() {
    // The inverse: a writable `mem` must be zero-initialized, not given contents.
    compile_err(
        "struct S { m: mem<u8, 4> = [1, 2, 3, 4] }",
        "must be zero-initialized",
    );
}

// --- include_hex! (ROM1b) --------------------------------------------------

/// Compile a `main.snt` + a `table.hex` written to a fresh temp directory,
/// returning the error string on failure (exercises `include_hex!`, which needs a
/// file-based project).
fn compile_with_hex(snt: &str, hex: &str) -> Result<sinter_compiler::Design, String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sinter-rom-hex-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("table.hex"), hex).expect("write hex");
    let root = dir.join("main.snt");
    std::fs::write(&root, snt).expect("write snt");
    sinter_compiler::compile_project(&root).map_err(|e| e.to_string())
}

const HEX_SNT: &str = "
    struct Rom { data: rom<u8, 8> = include_hex!(\"table.hex\") }
    impl Rom {
        #[clock(domain = sys)]
        fn tick(self, addr: u3) -> (Rom, u8) {
            let v = self.data.value();
            (Rom { data: self.data.next(addr) }, v)
        }
    }
";

#[test]
fn include_hex_loads_the_rom_contents() {
    // The committed example: bare + `0x`-prefixed words, comments, blank lines.
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/rom_hex/main.snt"
    );
    let design =
        sinter_compiler::compile_project(std::path::Path::new(root)).expect("compile project");
    let m = &design.modules[0];
    assert_eq!(
        m.mems[0].reset,
        vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
        "contents parsed from table.hex"
    );
    assert!(m.mem_writes.is_empty(), "a rom has no write port");
}

#[test]
fn include_hex_without_a_file_project_is_rejected() {
    // `compile_str` has no source directory to resolve against.
    compile_err(
        "struct S { r: rom<u8, 2> = include_hex!(\"x.hex\") }",
        "file-based project",
    );
}

#[test]
fn include_hex_with_a_bad_word_is_rejected() {
    let err = compile_with_hex(HEX_SNT, "0\n1\nzz\n3\n4\n5\n6\n7\n").unwrap_err();
    assert!(err.contains("not a hex word"), "got: {err}");
}

#[test]
fn include_hex_with_the_wrong_word_count_is_rejected() {
    // Three words for a depth-8 rom -> the shared rom size check fires.
    let err = compile_with_hex(HEX_SNT, "1\n2\n3\n").unwrap_err();
    assert!(err.contains("needs 8 words"), "got: {err}");
}
