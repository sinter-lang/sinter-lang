# Shared Verilator flags for the Sinter conformance oracle (DESIGN §7.4).
#
# Mirrors the downstream pico-gs integration config
# (londey/pico-gs:integration/verilator.f) so the compiler's generated
# SystemVerilog is exercised under the SAME Verilator settings as the project
# that consumes it. Waveform tracing (--trace-fst / --trace-structs) is omitted:
# the conformance harness writes its own binary trace for the bit-for-bit diff,
# not a Verilator FST waveform.
#
# This file drives the build / co-simulation step. The lint-clean guarantee
# (claude-skill-verilog) is enforced separately by a strict `--lint-only -Wall`
# pass with warnings fatal (this file keeps -Wno-fatal to match pico-gs's run).

# Warnings: report everything. Non-fatal here to match pico-gs; the separate
# strict lint pass is what fails the build on any warning.
-Wall
-Wno-fatal

# Parallel build.
-j 0

# SystemVerilog assertions.
--assert

# Timing constructs.
--timing

# X-propagation: surface missing resets / uninitialised signals.
--x-assign unique
--x-initial unique
