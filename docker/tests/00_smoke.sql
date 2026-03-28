-- Smoke test: verify all extensions loaded
SELECT extname, extversion FROM pg_extension WHERE extname IN ('postgis', 'h3', 'pg_accel') ORDER BY extname;

-- Verify PostGIS
SELECT ST_AsText(ST_MakePoint(0, 0));

-- Verify basic query
SELECT COUNT(*) FROM analytics_events WHERE value > 500;

-- Verify spatial
SELECT COUNT(*) FROM spatial_points WHERE ST_DWithin(
    geom,
    ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326),
    0.01
);
