//! VCD (Value Change Dump, IEEE 1364 §18) waveform writer for the reference
//! simulator, viewable in GTKWave / Surfer.
//!
//! One simulated cycle `k` spans `[10k, 10k + 10)` ns: the clock rises at
//! `10k`, every signal takes its cycle-`k` value there, and the clock falls at
//! `10k + 5`. The next rising edge samples those values, as in hardware.
//!
//! With several clock domains (DESIGN §6.7) there is a `clk_<d>` per domain.
//! Time is twice the simulator's (so every half period is an integer): a
//! domain's clock rises at each of its edges and falls half a period later.
//!
//! Signals are grouped into scopes under the module: `in` (input ports),
//! `out` (output ports), `reg` (registers, pre-edge value), and `wire` (named
//! combinational wires). Values are raw bit patterns; set a signed display
//! format in the viewer for `iN` signals.

use std::collections::HashMap;
use std::io::{self, Write};

use sinter_syntax::ast::Type;

use crate::error::mask;
use crate::ir::Module;
use crate::sim::Simulator;

/// Half the clock period, in the VCD's 1 ns timescale.
const HALF_PERIOD: u64 = 5;

/// Where a dumped signal's value comes from each cycle.
enum Source {
    /// [`Simulator::signal`] (inputs, registers, wires).
    Env(String),
    /// The output map returned by [`Simulator::step`].
    Output(String),
}

/// One dumped signal.
struct Signal {
    id: String,
    width: u32,
    source: Source,
    /// The last value written, to emit only changes.
    last: Option<u64>,
}

/// Streams a VCD waveform of a [`Module`], one call to [`VcdWriter::cycle`] per
/// simulated cycle.
pub struct VcdWriter<W: Write> {
    out: W,
    clk_id: String,
    signals: Vec<Signal>,
    cycles: u64,
    /// With several clock domains: each domain's clock id and period, and the
    /// pending falling edges `(time, domain)`.
    clocks: Vec<(String, u64)>,
    falls: Vec<(u64, usize)>,
}

impl<W: Write> VcdWriter<W> {
    /// Write the VCD header (signal declarations) for `module` to `out`.
    ///
    /// # Errors
    /// Returns any I/O error from writing to `out`.
    pub fn new(mut out: W, module: &Module) -> io::Result<Self> {
        let mut next_id = 0usize;
        let mut alloc = || {
            let id = vcd_id(next_id);
            next_id += 1;
            id
        };

        writeln!(out, "$version sinter {} $end", env!("CARGO_PKG_VERSION"))?;
        writeln!(out, "$timescale 1ns $end")?;
        writeln!(out, "$scope module {} $end", module.name)?;
        let clk_id = alloc();
        let mut clocks = Vec::new();
        if module.is_multi_clock() {
            let mut first = true;
            for d in &module.domains {
                let id = if first { clk_id.clone() } else { alloc() };
                first = false;
                writeln!(out, "$var wire 1 {id} clk_{} $end", d.name)?;
                clocks.push((id, d.period.max(1)));
            }
        } else {
            writeln!(out, "$var wire 1 {clk_id} clk $end")?;
        }

        let in_ports: Vec<_> = module
            .inputs()
            .map(|p| (p.name.clone(), p.ty, Source::Env(p.name.clone())))
            .collect();
        let out_ports: Vec<_> = module
            .outputs()
            .map(|p| (p.name.clone(), p.ty, Source::Output(p.name.clone())))
            .collect();
        let regs: Vec<_> = module
            .regs
            .iter()
            .map(|r| (r.name.clone(), r.ty, Source::Env(r.name.clone())))
            .collect();
        let wires: Vec<_> = module
            .lets
            .iter()
            .map(|l| (l.name.clone(), l.ty, Source::Env(l.name.clone())))
            .collect();

        let mut signals = Vec::new();
        for (scope, group) in [
            ("in", in_ports),
            ("out", out_ports),
            ("reg", regs),
            ("wire", wires),
        ] {
            if group.is_empty() {
                continue;
            }
            writeln!(out, "$scope module {scope} $end")?;
            for (name, ty, source) in group {
                let id = alloc();
                writeln!(out, "{}", var_decl(&id, &name, ty))?;
                signals.push(Signal {
                    id,
                    width: ty.width(),
                    source,
                    last: None,
                });
            }
            writeln!(out, "$upscope $end")?;
        }
        writeln!(out, "$upscope $end")?;
        writeln!(out, "$enddefinitions $end")?;

        Ok(VcdWriter {
            out,
            clk_id,
            signals,
            cycles: 0,
            clocks,
            falls: Vec::new(),
        })
    }

    /// Record one cycle: call right after [`Simulator::step`], passing the
    /// outputs it returned.
    ///
    /// # Errors
    /// Returns any I/O error from writing to the underlying writer.
    pub fn cycle(&mut self, sim: &Simulator<'_>, outputs: &HashMap<String, u64>) -> io::Result<()> {
        if !self.clocks.is_empty() {
            return self.multi_clock_step(sim, outputs);
        }
        let t = self.cycles * 2 * HALF_PERIOD;
        writeln!(self.out, "#{t}")?;
        if self.cycles == 0 {
            writeln!(self.out, "$dumpvars")?;
        }
        writeln!(self.out, "1{}", self.clk_id)?;
        self.write_values(sim, outputs)?;
        if self.cycles == 0 {
            writeln!(self.out, "$end")?;
        }
        writeln!(self.out, "#{}", t + HALF_PERIOD)?;
        writeln!(self.out, "0{}", self.clk_id)?;
        self.cycles += 1;
        Ok(())
    }

    /// Write each signal whose value changed since it was last written.
    fn write_values(
        &mut self,
        sim: &Simulator<'_>,
        outputs: &HashMap<String, u64>,
    ) -> io::Result<()> {
        for s in &mut self.signals {
            let raw = match &s.source {
                Source::Env(name) => sim.signal(name),
                Source::Output(name) => outputs.get(name).copied(),
            };
            let v = mask(raw.unwrap_or(0), s.width);
            if s.last != Some(v) {
                writeln!(self.out, "{}", value_change(v, s.width, &s.id))?;
                s.last = Some(v);
            }
        }
        Ok(())
    }

    /// One scheduler step of a multi-clock design: the falls due before it,
    /// then the ticking domains' rising edges and every changed value.
    fn multi_clock_step(
        &mut self,
        sim: &Simulator<'_>,
        outputs: &HashMap<String, u64>,
    ) -> io::Result<()> {
        let t = 2 * sim.time();
        self.write_falls_before(t + 1)?;
        writeln!(self.out, "#{t}")?;
        if self.cycles == 0 {
            writeln!(self.out, "$dumpvars")?;
        }
        for (d, &tick) in sim.ticking().iter().enumerate() {
            let (id, period) = &self.clocks[d];
            if tick {
                writeln!(self.out, "1{id}")?;
                self.falls.push((t + period, d));
            } else if self.cycles == 0 {
                writeln!(self.out, "0{id}")?;
            }
        }
        self.write_values(sim, outputs)?;
        if self.cycles == 0 {
            writeln!(self.out, "$end")?;
        }
        self.cycles += 1;
        Ok(())
    }

    /// Write every pending falling clock edge at a time before `limit`, in
    /// time order.
    fn write_falls_before(&mut self, limit: u64) -> io::Result<()> {
        self.falls.sort_unstable();
        let due: Vec<_> = self
            .falls
            .iter()
            .copied()
            .filter(|&(t, _)| t < limit)
            .collect();
        self.falls.retain(|&(t, _)| t >= limit);
        let mut last = None;
        for (t, d) in due {
            if last != Some(t) {
                writeln!(self.out, "#{t}")?;
                last = Some(t);
            }
            writeln!(self.out, "0{}", self.clocks[d].0)?;
        }
        Ok(())
    }

    /// Write the closing clock edge (which samples the last cycle) and flush.
    ///
    /// # Errors
    /// Returns any I/O error from writing to or flushing the underlying writer.
    pub fn finish(mut self) -> io::Result<W> {
        if !self.clocks.is_empty() {
            self.write_falls_before(u64::MAX)?;
            self.out.flush()?;
            return Ok(self.out);
        }
        writeln!(self.out, "#{}", self.cycles * 2 * HALF_PERIOD)?;
        writeln!(self.out, "1{}", self.clk_id)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// The `$var` declaration for one signal. Multi-bit signals carry a `[msb:0]`
/// range so viewers show the bus width.
fn var_decl(id: &str, name: &str, ty: Type) -> String {
    let w = ty.width();
    if w == 1 {
        format!("$var wire 1 {id} {name} $end")
    } else {
        format!("$var wire {w} {id} {name} [{}:0] $end", w - 1)
    }
}

/// One value-change line: `0!` for a scalar, `b1010 !` for a vector.
fn value_change(v: u64, width: u32, id: &str) -> String {
    if width == 1 {
        format!("{v}{id}")
    } else {
        format!("b{v:0w$b} {id}", w = width as usize)
    }
}

/// The `n`th short VCD identifier, drawn from the printable ASCII range
/// `!`..=`~` (base 94).
fn vcd_id(mut n: usize) -> String {
    const FIRST: u8 = b'!';
    const COUNT: usize = 94;
    let mut id = Vec::new();
    loop {
        // `n % COUNT < 94`, so this always fits in a `u8`.
        id.push(FIRST + u8::try_from(n % COUNT).unwrap_or(0));
        n /= COUNT;
        if n == 0 {
            break;
        }
        n -= 1;
    }
    String::from_utf8_lossy(&id).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_str;

    const COUNTER: &str = "
        struct C { pub value: u4 = 0, }
        impl C {
            #[clock(domain = sys)]
            fn tick(self, en: u1) -> C {
                C { value: if en == 1 { (self.value + 1) as u4 wrap } else { self.value } }
            }
        }
    ";

    #[test]
    fn ids_are_unique_and_printable() {
        let ids: Vec<_> = (0..10_000).map(vcd_id).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        assert!(ids
            .iter()
            .all(|id| id.bytes().all(|b| (b'!'..=b'~').contains(&b))));
        assert_eq!(vcd_id(0), "!");
        assert_eq!(vcd_id(93), "~");
        assert_eq!(vcd_id(94).len(), 2);
    }

    #[test]
    fn dumps_header_and_value_changes() {
        let design = compile_str(COUNTER).expect("compiles");
        let module = &design.modules[0];
        let mut sim = Simulator::new(module);
        let mut vcd = VcdWriter::new(Vec::new(), module).expect("header");
        for en in [1, 1, 0] {
            let inputs = HashMap::from([("en".to_string(), en)]);
            let out = sim.step(&inputs);
            vcd.cycle(&sim, &out).expect("cycle");
        }
        let text = String::from_utf8(vcd.finish().expect("finish")).expect("utf8");

        // Header: clk, then the in / out / reg scopes (no wires here).
        assert!(text.contains("$timescale 1ns $end"));
        assert!(text.contains("$var wire 1 ! clk $end"));
        assert!(text.contains("$scope module in $end\n$var wire 1 \" en $end"));
        assert!(text.contains("$var wire 4 # value_q [3:0] $end"));
        assert!(text.contains("$scope module reg $end\n$var wire 4 $ value [3:0] $end"));
        assert!(!text.contains("$scope module wire $end"));

        // Cycle 0 dumps everything; later cycles only changes. `value` counts
        // 0, 1, 2 over the three cycles (en = 1, 1, 0), and `en` drops at 20.
        let body = &text[text.find("#0").expect("time 0")..];
        assert_eq!(
            body,
            "#0\n$dumpvars\n1!\n1\"\nb0000 #\nb0000 $\n$end\n#5\n0!\n\
             #10\n1!\nb0001 #\nb0001 $\n#15\n0!\n\
             #20\n1!\n0\"\nb0010 #\nb0010 $\n#25\n0!\n\
             #30\n1!\n"
        );
    }
}
