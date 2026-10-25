//! Generator for the Verilator C++ testbench harness.
//!
//! The harness drives the Verilated DUT with the **same** binary stimulus the
//! Rust reference model consumes and writes its outputs in the **same** binary
//! trace format (8 little-endian bytes per value, ports in declaration order).
//! Diffing the two traces is the conformance oracle (DESIGN §7.4).
//!
//! The cycle protocol matches `sim.rs`: sample outputs while the clock is low
//! (combinational settle), then pulse the posedge to commit next-state.

use std::fmt::Write as _;

use crate::ir::Module;

/// Emit `main.cpp` for the Verilator testbench of `m`.
///
/// Invoked as: `./sim <stimulus.bin> <trace.bin> <cycles>`.
#[must_use]
pub fn emit_cpp_harness(m: &Module) -> String {
    if m.is_multi_clock() {
        return emit_multi_clock_harness(m);
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Auto-generated Verilator harness for module `{}`.",
        m.name
    );
    let _ = writeln!(out, "#include <cstdint>");
    let _ = writeln!(out, "#include <cstdio>");
    let _ = writeln!(out, "#include <cstdlib>");
    let _ = writeln!(out, "#include \"V{}.h\"", m.name);
    let _ = writeln!(out, "#include \"verilated.h\"");
    let _ = writeln!(out);
    let _ = writeln!(out, "int main(int argc, char** argv) {{");
    let _ = writeln!(out, "    if (argc < 4) {{");
    let _ = writeln!(
        out,
        "        std::fprintf(stderr, \"usage: %s <stimulus.bin> <trace.bin> <cycles>\\n\", argv[0]);"
    );
    let _ = writeln!(out, "        return 2;");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    Verilated::commandArgs(argc, argv);");
    let _ = writeln!(out, "    const char* stim_path = argv[1];");
    let _ = writeln!(out, "    const char* trace_path = argv[2];");
    let _ = writeln!(out, "    long cycles = std::strtol(argv[3], nullptr, 10);");
    let _ = writeln!(out);
    let _ = writeln!(out, "    V{}* top = new V{}();", m.name, m.name);
    let _ = writeln!(out, "    std::FILE* fin = std::fopen(stim_path, \"rb\");");
    let _ = writeln!(out, "    std::FILE* fout = std::fopen(trace_path, \"wb\");");
    let _ = writeln!(out, "    if (!fin || !fout) {{");
    let _ = writeln!(
        out,
        "        std::fprintf(stderr, \"cannot open io files\\n\");"
    );
    let _ = writeln!(out, "        return 3;");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    uint64_t tmp = 0;");
    let _ = writeln!(out, "    (void)tmp;");
    let _ = writeln!(out);
    // Assert the asynchronous active-low reset with a falling edge and no
    // clock edge: memories aren't reset, so nothing may write them before
    // cycle 0 (the simulator starts there, from the reset state).
    let _ = writeln!(
        out,
        "    // Reset: a falling edge of rst_n, with no clock edge."
    );
    let _ = writeln!(out, "    top->clk = 0; top->rst_n = 1; top->eval();");
    let _ = writeln!(out, "    top->rst_n = 0; top->eval();");
    let _ = writeln!(out);
    let _ = writeln!(out, "    for (long k = 0; k < cycles; ++k) {{");
    let _ = writeln!(out, "        top->rst_n = 1;");
    for p in m.inputs() {
        let _ = writeln!(
            out,
            "        if (std::fread(&tmp, sizeof(uint64_t), 1, fin) != 1) break;"
        );
        let _ = writeln!(out, "        top->{0} = (decltype(top->{0}))tmp;", p.name);
    }
    let _ = writeln!(
        out,
        "        // Settle combinational logic with held state."
    );
    let _ = writeln!(out, "        top->clk = 0; top->eval();");
    for p in m.outputs() {
        let _ = writeln!(out, "        tmp = (uint64_t)top->{};", p.name);
        let _ = writeln!(out, "        std::fwrite(&tmp, sizeof(uint64_t), 1, fout);");
    }
    let _ = writeln!(out, "        // Commit next-state on the rising edge.");
    let _ = writeln!(out, "        top->clk = 1; top->eval();");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(out, "    std::fclose(fin);");
    let _ = writeln!(out, "    std::fclose(fout);");
    let _ = writeln!(out, "    delete top;");
    let _ = writeln!(out, "    return 0;");
    let _ = writeln!(out, "}}");
    out
}

/// The harness for a design with several clock domains (DESIGN §6.7). It
/// replays the simulator's edge schedule: each step advances to the earliest
/// pending clock edge, applies new inputs only to the domains with an edge
/// then, records the settled outputs, and pulses those domains' clocks
/// together. Before the schedule it holds `rst_n` low, releases it, and gives
/// every clock two edges to clear its domain's reset synchronizer, so the
/// Verilog starts from the same post-reset state as the simulator.
fn emit_multi_clock_harness(m: &Module) -> String {
    let mut out = String::new();
    let n = m.domains.len();
    let periods: Vec<String> = m
        .domains
        .iter()
        .map(|d| format!("{}ULL", d.period))
        .collect();
    let set_clocks = |out: &mut String, value: &str| {
        for d in &m.domains {
            let _ = writeln!(out, "        top->clk_{} = {value};", d.name);
        }
    };
    let _ = writeln!(
        out,
        "// Auto-generated Verilator harness for module `{}` ({n} clock domains).",
        m.name
    );
    let _ = writeln!(out, "#include <cstdint>");
    let _ = writeln!(out, "#include <cstdio>");
    let _ = writeln!(out, "#include <cstdlib>");
    let _ = writeln!(out, "#include \"V{}.h\"", m.name);
    let _ = writeln!(out, "#include \"verilated.h\"");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "static const uint64_t PERIOD[{n}] = {{{}}};",
        periods.join(", ")
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "int main(int argc, char** argv) {{");
    let _ = writeln!(out, "    if (argc < 4) {{");
    let _ = writeln!(
        out,
        "        std::fprintf(stderr, \"usage: %s <stimulus.bin> <trace.bin> <steps>\\n\", argv[0]);"
    );
    let _ = writeln!(out, "        return 2;");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    Verilated::commandArgs(argc, argv);");
    let _ = writeln!(out, "    long steps = std::strtol(argv[3], nullptr, 10);");
    let _ = writeln!(out, "    V{0}* top = new V{0}();", m.name);
    let _ = writeln!(out, "    std::FILE* fin = std::fopen(argv[1], \"rb\");");
    let _ = writeln!(out, "    std::FILE* fout = std::fopen(argv[2], \"wb\");");
    let _ = writeln!(out, "    if (!fin || !fout) {{");
    let _ = writeln!(
        out,
        "        std::fprintf(stderr, \"cannot open io files\\n\");"
    );
    let _ = writeln!(out, "        return 3;");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    uint64_t tmp = 0;");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    // Reset, then two edges per clock to clear the reset synchronizers."
    );
    let _ = writeln!(out, "    top->rst_n = 1;");
    let _ = writeln!(out, "    {{");
    set_clocks(&mut out, "0");
    let _ = writeln!(out, "        top->eval();");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    top->rst_n = 0;");
    let _ = writeln!(out, "    {{");
    set_clocks(&mut out, "0");
    let _ = writeln!(out, "        top->eval();");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    top->rst_n = 1;");
    let _ = writeln!(out, "    for (int e = 0; e < 2; ++e) {{");
    set_clocks(&mut out, "1");
    let _ = writeln!(out, "        top->eval();");
    set_clocks(&mut out, "0");
    let _ = writeln!(out, "        top->eval();");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(out, "    uint64_t next[{n}] = {{0}};");
    let _ = writeln!(out, "    for (long k = 0; k < steps; ++k) {{");
    let _ = writeln!(out, "        uint64_t t = next[0];");
    let _ = writeln!(out, "        for (int d = 1; d < {n}; ++d) {{");
    let _ = writeln!(out, "            if (next[d] < t) t = next[d];");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "        bool tick[{n}];");
    let _ = writeln!(
        out,
        "        for (int d = 0; d < {n}; ++d) tick[d] = next[d] == t;"
    );
    let _ = writeln!(
        out,
        "        // New inputs only for the domains with an edge now; the rest hold."
    );
    for p in m.inputs() {
        let _ = writeln!(
            out,
            "        if (std::fread(&tmp, sizeof(uint64_t), 1, fin) != 1) break;"
        );
        let _ = writeln!(
            out,
            "        if (tick[{}]) top->{1} = (decltype(top->{1}))tmp;",
            p.domain, p.name
        );
    }
    let _ = writeln!(out, "        top->eval();");
    for p in m.outputs() {
        let _ = writeln!(out, "        tmp = (uint64_t)top->{};", p.name);
        let _ = writeln!(out, "        std::fwrite(&tmp, sizeof(uint64_t), 1, fout);");
    }
    let _ = writeln!(
        out,
        "        // Pulse the clocks of the ticking domains together."
    );
    for (d, dom) in m.domains.iter().enumerate() {
        let _ = writeln!(out, "        top->clk_{} = tick[{d}] ? 1 : 0;", dom.name);
    }
    let _ = writeln!(out, "        top->eval();");
    set_clocks(&mut out, "0");
    let _ = writeln!(out, "        top->eval();");
    let _ = writeln!(out, "        for (int d = 0; d < {n}; ++d) {{");
    let _ = writeln!(out, "            if (tick[d]) next[d] += PERIOD[d];");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(out, "    std::fclose(fin);");
    let _ = writeln!(out, "    std::fclose(fout);");
    let _ = writeln!(out, "    delete top;");
    let _ = writeln!(out, "    return 0;");
    let _ = writeln!(out, "}}");
    out
}
