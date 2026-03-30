class PgAccel < Formula
  desc "GPU-accelerated query processing for PostgreSQL"
  homepage "https://github.com/yocontra/pg_accel"
  url "https://github.com/yocontra/pg_accel/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "PLACEHOLDER" # TODO: replace with real sha256 after first release
  license "PostgreSQL"

  depends_on "rust" => :build
  depends_on "cmake" => :build
  depends_on "postgresql@17"

  def install
    # Ensure cargo-pgrx is available
    system "cargo", "install", "cargo-pgrx@0.17.0", "--locked"

    pg_config = Formula["postgresql@17"].opt_bin/"pg_config"

    # Initialize pgrx with the Homebrew PG 17
    system "cargo", "pgrx", "init", "--pg17", pg_config

    # Build the extension package (CPU-fallback; GPU requires AdaptiveCpp)
    system "cargo", "pgrx", "package",
           "--package", "pg_accel",
           "--pg-config", pg_config,
           "--no-default-features",
           "--features", "pg17"

    # Install extension files into the PG extension directory
    pg_sharedir = Utils.safe_popen_read(pg_config, "--sharedir").chomp
    pg_pkglibdir = Utils.safe_popen_read(pg_config, "--pkglibdir").chomp

    # The pgrx package output mirrors the install tree under target/release/pg_accel-pg17
    pkg_root = buildpath/"target/release/pg_accel-pg17"

    # Find and install the shared library
    lib.install Dir[pkg_root/"**/*.so"]
    lib.install Dir[pkg_root/"**/*.dylib"]

    # Install .control and SQL files to PG's extension directory
    Dir[pkg_root/"**/extension/*.control"].each do |f|
      (share/"postgresql@17/extension").install f
    end
    Dir[pkg_root/"**/extension/*.sql"].each do |f|
      (share/"postgresql@17/extension").install f
    end
  end

  def post_install
    pg_pkglibdir = Utils.safe_popen_read(
      Formula["postgresql@17"].opt_bin/"pg_config", "--pkglibdir"
    ).chomp
    pg_sharedir = Utils.safe_popen_read(
      Formula["postgresql@17"].opt_bin/"pg_config", "--sharedir"
    ).chomp

    # Symlink .so/.dylib into PG's pkglibdir
    Dir[lib/"*.so", lib/"*.dylib"].each do |f|
      ln_sf f, pg_pkglibdir/File.basename(f)
    end

    # Symlink .control and .sql into PG's extension dir
    ext_dir = Pathname.new(pg_sharedir)/"extension"
    Dir[share/"postgresql@17/extension/*"].each do |f|
      ln_sf f, ext_dir/File.basename(f)
    end
  end

  def caveats
    <<~EOS
      pg_accel has been installed with CPU-only batched evaluation.

      For GPU acceleration (Apple Silicon with Metal), you must also install
      AdaptiveCpp and rebuild with the `gpu` feature:

        # See: just setup-gpu (in the pg_accel repo)
        # Or: https://github.com/yocontra/pg_accel#gpu-acceleration

      To enable the extension, add to postgresql.conf:

        shared_preload_libraries = 'pg_accel'

      Then restart PostgreSQL and run:

        CREATE EXTENSION pg_accel;
    EOS
  end

  test do
    system Formula["postgresql@17"].opt_bin/"pg_config", "--version"
  end
end
