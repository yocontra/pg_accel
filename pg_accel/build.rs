//! Build script for `pg_accel`.
//!
//! When the `gpu` feature is enabled, invokes `CMake` to build `libpgaccel_kernels`
//! from the sibling `pgaccel-kernels/` directory and emits the appropriate
//! `cargo:rustc-link-*` directives so the Rust FFI bridge can find it.
//!
//! When the `gpu` feature is **not** enabled, this script is a no-op — there is
//! no C++ dependency at all.

fn main() {
    // Re-run if the feature set changes.
    println!("cargo::rerun-if-changed=build.rs");

    #[cfg(feature = "gpu")]
    gpu_build::build_kernels();
}

#[cfg(feature = "gpu")]
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
            // SYCL is optional — CMake will warn and fall back to CPU-only if
            // AdaptiveCpp is not installed.
            .arg("-DPGACCEL_USE_SYCL=ON");

        // AdaptiveCpp installs to ~/local via `just setup-gpu`.
        if acpp_prefix.join("lib/cmake/AdaptiveCpp").exists() {
            cmd.arg(format!("-DCMAKE_PREFIX_PATH={}", acpp_prefix.display()));
            // Target OMP (CPU) by default; Metal/CUDA selected at runtime.
            // Explicit target avoids concatenated default "ompmetal" bug.
            cmd.arg("-DACPP_TARGETS=omp");

            // On macOS, LLVM's clang needs the Homebrew libomp path.
            if cfg!(target_os = "macos") {
                if let Some(libomp) = find_brew_prefix("libomp") {
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
        // CMake places the shared library directly in the build directory by default.
        println!("cargo::rustc-link-search=native={}", build_dir.display());
        println!("cargo::rustc-link-lib=dylib=pgaccel_kernels");

        // On macOS, also set an rpath so the dylib is found at runtime.
        if cfg!(target_os = "macos") {
            println!("cargo::rustc-link-arg=-Wl,-rpath,{}", build_dir.display());
        }
    }
}
