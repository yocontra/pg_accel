# Task 0 actual-compiler feasibility characterization

2026-10-12. Diagnostic only: no projection receipt consumer, analyzer approval,
installed toolchain mutation or native CMake wiring has been introduced.

## Actual chain

CMake RULE_LAUNCH_COMPILE invokes installed syclcc-launcher, which substitutes
acpp for nominal /usr/bin/c++. The installed JSON selects LLVM 20 clang++ and
generic SSCP. `resolved-command.txt` retains its actual injected macros,
hcf_registration.hpp, -acpp-sscp, fast-math kernel options and frontend/pass
plugin paths. Shared compilation defines PGACCEL_TEST_HOOKS; static does not.

`characterize.sh` attempted normal -save-temps=obj through that actual chain.
Original session60638 terminated1: the split backend stage rejected the SSCP
options because their plugin registration was absent. Failure is retained in
first/compile.log. No generated or installed file was changed.

`replay.py` then ran two identical ordinary object compilations and two identical
plugin-generated LLVM IR compilations, with outputs confined here. Original
session26250 terminated0. `replays.json` binds exact arguments, sizes and hashes.
The real production join_rows.cpp was read, not modified. No GPU work ran.

## Differences

Normal objects:599968 bytes each; first SHA256
134dfda22692ab1e160997ab2647d934174d4e211ef4846d4ebed11a89bce85a,
second78b95908db485a4010c95133c51ae07f6fd0839d2f2456a48ffd163cc7f15b1a.
Their HCF IDs are1349092324817092773 and14091942912625519846.
Mach-O __TEXT,__text differs at480bytes; __StaticInit differs at1byte;
__TEXT,__const lengths are156396/156397 because the decimal ID widths differ.
Other inspected file-backed sections match. Byte inequality is real executable
and embedded-artifact inequality, not merely an ignorable debug timestamp.

Pinned TargetSeparationPass.cpp defines random_device/mt19937 at136–137, calls
generateRandomNumber at922, serializes object-id at837 and replaces host-side
HCF-ID globals at980. No existing deterministic-ID option was found.
HostKernelNameExtractionPass.cpp:38 collects functions in SmallPtrSet<...,16>
and iterates it at51 while IRConstant::set appends globals. This introduces
pointer-order-dependent global creation when the collection grows.

Both emitted host IRs have18 kernel-name .initialized globals and48 signed HCF-ID
operand occurrences. For diagnosis only, replacing the two known actual IDs
and sorting these18 declarations makes their complete texts equal. This is NOT
an implemented normalization or approval rule; all object bytes remain required.

The embedded device bitcode is87728bytes with SHA256
a5d2a5ea2d48e6e1776078889ace93c547c992adea7b59b663e17c5926b9af52
across ordinary-object and emitted-IR actions. `extract_payload.py` extracts and
disassembles it. An initial diagnostic decoder incorrectly handled LLVM literal
backslash escapes and falsely suggested an IR-action payload difference;
llvm-dis rejected that extraction with "can't skip to bit815138400 from352512".
The corrected exact decoder handles both hex escapes and literal backslashes;
both extracted payloads now disassemble and match byte-for-byte. The only
llvm-dis text difference is its printed input filename in ModuleID.

## Required reviewed design extension

An explicit fresh per-object ID option, supplied identically to original compile
and replay, plus stable kernel-name global creation order is the smallest
identified path to strict replay. Default absence should preserve runtime's
existing random-ID behavior. No byte may be discarded merely because it differs.
Actual object replay must establish sufficiency; the present diagnosis is not
that proof. A plan review and toolchain-patch ownership assignment are needed
before implementation outside the approved four scripts/build wiring scope.

Plugin-only building is possible: existing acpp-clang MODULE consists of18
compiler/frontend and19 CBS objects; it uses installed LLVM20 and dynamic_lookup
on macOS. A new owned CMake configuration can build --target acpp-clang without
rebuilding LLVM, runtime or device bitcode. Full fresh plugin build cost is not
measured. Existing generated flags/link instructions were read only; no installed
launcher, plugin, source checkout, build directory, PostgreSQL module or cache
was modified. Task0 remains incomplete pending that reviewed extension.
