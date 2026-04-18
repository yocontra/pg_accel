//! Build script for `pg_accel`.
//!
//! Invokes `CMake` to build `libpgaccel_kernels` from the sibling
//! `pgaccel-kernels/` directory and emits the appropriate
//! `cargo:rustc-link-*` directives so the Rust FFI bridge can find it.
//! GPU linkage is unconditional — AdaptiveCpp/SYCL is the only supported
//! backend and there is no CPU-fallback build mode.

// Build scripts are not library code — panicking on missing env vars or
// cmake failures is the correct behaviour.
#![allow(clippy::expect_used)]

fn main() {
    // Re-run if the build script changes.
    println!("cargo::rerun-if-changed=build.rs");

    gpu_build::build_kernels();

    // macOS Sequoia+ (26.x) eagerly resolves undefined symbols in flat
    // namespace binaries at dyld load time, which breaks pgrx test binaries
    // that reference PG functions via `-undefined dynamic_lookup`. Generate
    // a stub dylib exporting all PG symbols and link it into test binaries
    // only (never the production cdylib) so dyld finds a dummy definition
    // at load time. The stubs are never called: real implementations come
    // from postgres itself when the extension .so is dlopened.
    #[cfg(target_os = "macos")]
    pg_stub::build_stub();
}

#[cfg(target_os = "macos")]
mod pg_stub {
    use std::path::PathBuf;
    use std::process::Command;

    /// Generate a Rust source file with `#[no_mangle]` stubs for every
    /// global symbol exported by the `postgres` executable.
    ///
    /// Background: macOS Sequoia+ (26.x) eagerly resolves undefined data
    /// symbol references at dyld load time, even with `-undefined
    /// dynamic_lookup` + `-no_fixup_chains`. pgrx lib unit test binaries
    /// inherit hundreds of PG symbol references (e.g. static data like
    /// `CheckXidAlive`), and dyld aborts before the test runner starts.
    ///
    /// The generated file is `include!`'d from `src/pg_stubs.rs` under
    /// `cfg(all(test, target_os = "macos"))`, so the stubs are compiled
    /// into the test binary only — NEVER into the production cdylib that
    /// postgres dlopens. Real implementations always come from postgres
    /// itself at runtime; the stubs exist purely to satisfy the loader.
    pub fn build_stub() {
        let pg_config_path = std::env::var("PGRX_PG_CONFIG_PATH")
            .or_else(|_| std::env::var("PG_CONFIG"))
            .unwrap_or_else(|_| "pg_config".to_string());

        let Some(bindir) = Command::new(&pg_config_path)
            .arg("--bindir")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        else {
            return;
        };

        let postgres_bin = PathBuf::from(&bindir).join("postgres");
        if !postgres_bin.exists() {
            return;
        }

        println!("cargo::rerun-if-changed={}", postgres_bin.display());

        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
        let stub_rs = out_dir.join("pg_stubs_generated.rs");

        // Always emit the path so `include!` resolves even if we bail.
        println!("cargo::rustc-env=PG_STUBS_GENERATED={}", stub_rs.display());

        // Extract all global T (text/function), D (data), and S (other)
        // symbols. Skip U (undefined), compiler-internal (leading double
        // underscore), and namespaced (contains '.').
        let nm = Command::new("nm")
            .arg("-gP")
            .arg(&postgres_bin)
            .output()
            .expect("nm failed");
        assert!(nm.status.success(), "nm on postgres binary failed");
        let nm_out = String::from_utf8_lossy(&nm.stdout);

        let mut funcs: Vec<&str> = Vec::new();
        let mut datas: Vec<&str> = Vec::new();
        for line in nm_out.lines() {
            let mut parts = line.split_whitespace();
            let (Some(name), Some(ty)) = (parts.next(), parts.next()) else {
                continue;
            };
            // macOS nm prefixes with underscore; strip it so our
            // #[no_mangle] (which re-adds it on mach-o) matches.
            let clean = name.strip_prefix('_').unwrap_or(name);
            if clean.is_empty() || clean.starts_with("__") || !is_rust_ident(clean) {
                continue;
            }
            // Skip Rust keywords — we'd need r# prefix and that's noise.
            if is_rust_keyword(clean) {
                continue;
            }
            // Reserved / auto-provided symbols that would clash with the
            // test binary's own main + mach-o header.
            if matches!(
                clean,
                "main" | "_main" | "mh_execute_header" | "_mh_execute_header" | "start"
            ) {
                continue;
            }
            // Symbols we provide manually as passthrough implementations
            // in `src/pg_stubs.rs` (needed for standalone unit tests
            // that exercise code paths which hit PG FFI).
            if is_manual_stub(clean) {
                continue;
            }
            match ty {
                "T" => funcs.push(clean),
                "D" | "S" => datas.push(clean),
                _ => {}
            }
        }

        // Stub generation rules:
        //
        // - T (text/function): emit an `extern "C-unwind" fn NAME() -> usize`
        //   that returns 0. Lives in .text (executable), so calls through
        //   pgrx's `extern "C-unwind"` declarations won't SIGBUS on jump.
        //   Returns null/zero in x0 — callers that check for null handle
        //   it gracefully; everything else is undefined but never hit in
        //   practice because the guarding `pg_guard_ffi_boundary` path
        //   only reads the function pointer after touching data stubs.
        //
        // - D/S (data): emit `static mut NAME: [u64; 16] = [0; 16]` — 128
        //   bytes of mutable, zero-initialized storage. pgrx's guard
        //   writes things like `CurrentMemoryContext = ...` and
        //   `PG_exception_stack = ...` to these addresses; read-only
        //   stubs would SIGBUS. 128 bytes fits any PG extern static we
        //   might reference.
        let mut rs = String::with_capacity(1 << 20);
        rs.push_str("// AUTO-GENERATED PG symbol stubs. Do not edit.\n");
        rs.push_str("// Satisfies macOS Sequoia+ dyld at load time; never executed.\n\n");
        for f in &funcs {
            rs.push_str("#[unsafe(no_mangle)]\npub extern \"C-unwind\" fn ");
            rs.push_str(f);
            rs.push_str("() -> usize { 0 }\n");
        }
        for d in &datas {
            rs.push_str("#[unsafe(no_mangle)]\npub static mut ");
            rs.push_str(d);
            rs.push_str(": [u64; 16] = [0; 16];\n");
        }

        let needs_write = std::fs::read_to_string(&stub_rs).map_or(true, |e| e != rs);
        if needs_write {
            std::fs::write(&stub_rs, &rs).expect("write pg_stubs_generated.rs");
        }
    }

    /// True if `s` is a valid Rust identifier (ASCII-only, per PG conventions).
    fn is_rust_ident(s: &str) -> bool {
        let mut chars = s.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Keep these out of the auto-generated stub file; `src/pg_stubs.rs`
    /// defines real passthrough implementations so unit tests that run
    /// outside postgres can exercise the surrounding code paths.
    fn is_manual_stub(s: &str) -> bool {
        matches!(
            s,
            "pg_detoast_datum"
                | "pg_detoast_datum_copy"
                | "pg_detoast_datum_packed"
                | "pg_detoast_datum_slice"
        )
    }

    /// Rust reserved words that would collide with `pub static NAME`.
    fn is_rust_keyword(s: &str) -> bool {
        matches!(
            s,
            "as" | "break"
                | "const"
                | "continue"
                | "crate"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "async"
                | "await"
                | "dyn"
                | "abstract"
                | "become"
                | "box"
                | "do"
                | "final"
                | "macro"
                | "override"
                | "priv"
                | "typeof"
                | "unsized"
                | "virtual"
                | "yield"
                | "try"
                | "union"
        )
    }
}

mod gpu_build {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Build `pgaccel-kernels` via CMake and emit linker directives.
    pub fn build_kernels() {
        let manifest_dir =
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
        let kernels_src = manifest_dir
            .parent()
            .expect("manifest dir has no parent")
            .join("pgaccel-kernels");

        assert!(
            kernels_src.join("CMakeLists.txt").exists(),
            "pgaccel-kernels source directory not found at {}",
            kernels_src.display(),
        );

        // Re-run when the kernel sources or build definition change.
        println!(
            "cargo::rerun-if-changed={}",
            kernels_src.join("CMakeLists.txt").display()
        );
        println!(
            "cargo::rerun-if-changed={}",
            kernels_src.join("src").display()
        );
        println!(
            "cargo::rerun-if-changed={}",
            kernels_src.join("include").display()
        );

        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
        let build_dir = out_dir.join("pgaccel-kernels-build");
        std::fs::create_dir_all(&build_dir).expect("failed to create cmake build directory");

        cmake_configure(&kernels_src, &build_dir);
        cmake_build(&build_dir);

        // The built shared library lives in the build directory.
        emit_link_directives(&build_dir);
    }

    fn cmake_configure(source_dir: &Path, build_dir: &Path) {
        let acpp_prefix = home_dir().join("local");

        let mut cmd = Command::new("cmake");
        cmd.arg("-S")
            .arg(source_dir)
            .arg("-B")
            .arg(build_dir)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            // AdaptiveCpp/SYCL is the sole GPU backend (CUDA/ROCm/L0/Metal/CPU).
            .arg("-DPGACCEL_USE_SYCL=ON");

        // AdaptiveCpp installs to ~/local via `just setup-gpu`.
        if acpp_prefix.join("lib/cmake/AdaptiveCpp").exists() {
            cmd.arg(format!("-DCMAKE_PREFIX_PATH={}", acpp_prefix.display()));
            // Target generic JIT compilation (works with Metal on macOS).
            // Explicit target avoids concatenated default "ompmetal" bug.
            cmd.arg("-DACPP_TARGETS=generic");

            // On macOS, use Homebrew LLVM (required by AdaptiveCpp) and
            // point the compiler at the Homebrew libomp headers/libs.
            if cfg!(target_os = "macos") {
                if let Some(llvm) = find_brew_prefix("llvm@20") {
                    cmd.arg(format!("-DCMAKE_C_COMPILER={llvm}/bin/clang"));
                    cmd.arg(format!("-DCMAKE_CXX_COMPILER={llvm}/bin/clang++"));
                }
                if let Some(libomp) = find_brew_prefix("libomp") {
                    cmd.arg(format!("-DCMAKE_CXX_FLAGS=-O2 -I{libomp}/include"));
                    let lib_flag = format!("-L{libomp}/lib");
                    cmd.arg(format!("-DCMAKE_SHARED_LINKER_FLAGS={lib_flag}"));
                    cmd.arg(format!("-DCMAKE_EXE_LINKER_FLAGS={lib_flag}"));
                }
            }
        }

        let status = cmd
            .status()
            .expect("failed to execute cmake — is cmake installed?");

        assert!(status.success(), "cmake configure step failed");
    }

    fn home_dir() -> PathBuf {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
    }

    fn find_brew_prefix(pkg: &str) -> Option<String> {
        Command::new("brew")
            .arg("--prefix")
            .arg(pkg)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn cmake_build(build_dir: &Path) {
        let status = Command::new("cmake")
            .arg("--build")
            .arg(build_dir)
            .arg("--config")
            .arg("Release")
            .arg("--parallel")
            .status()
            .expect("failed to execute cmake --build");

        assert!(status.success(), "cmake build step failed");
    }

    fn emit_link_directives(build_dir: &Path) {
        // CMake places the static library directly in the build directory.
        println!("cargo::rustc-link-search=native={}", build_dir.display());
        println!("cargo::rustc-link-lib=static=pgaccel_kernels");

        // AdaptiveCpp SSCP runtime — our static kernels reference hipsycl
        // symbols resolved by these shared libs at load time.
        let acpp_lib = home_dir().join("local/lib");
        println!("cargo::rustc-link-search=native={}", acpp_lib.display());
        println!("cargo::rustc-link-lib=dylib=acpp-rt");
        println!("cargo::rustc-link-lib=dylib=acpp-common");
        // Embed rpath so test binaries + the cdylib find the dylibs.
        println!("cargo::rustc-link-arg=-Wl,-rpath,{}", acpp_lib.display());

        // Link C++ standard library for static C++ code.
        if cfg!(target_os = "macos") {
            println!("cargo::rustc-link-lib=c++");
        } else {
            println!("cargo::rustc-link-lib=stdc++");
        }
    }
}
