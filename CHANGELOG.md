# Changelog

## [0.1.0] - 2026-03-28

### Added
- Custom Scan Provider for batch-parallel query execution
- GPU-accelerated spatial predicates (ST_Intersects, ST_Contains, ST_Within, ST_DWithin, ST_Distance)
- Three-layer spatial pipeline: bbox filter -> GPU geometric predicate -> CPU recheck
- H3 hexagonal index operations (h3_latlng_to_cell, h3_grid_distance, h3_cell_to_parent, h3_get_resolution)
- Raster operations (ST_MapAlgebra, ST_Clip, ST_Reclass) via GPU
- PostgreSQL built-in function batching (math, text, timestamp, JSON)
- Adapter system for third-party extension support
- GSERIALIZED geometry extractor (bbox, point extraction)
- PostGIS raster WKB format parser
- Thread budget management via shared memory LWLock
- Zero-overhead passthrough when GPU is not available
- GUC configuration: pg_accel.enabled, pg_accel.gpu_enabled, pg_accel.cost_multiplier
- pg_accel_device_info() and pg_accel_stats() monitoring functions
- Support for PostgreSQL 15, 16, 17, 18
- Support for PostGIS 3.3+, h3-pg 4.0+
- Apple Metal GPU support via AdaptiveCpp/SYCL
- CUDA, ROCm, Level Zero GPU support via AdaptiveCpp/SYCL
