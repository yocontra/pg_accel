-- 100K random points in NYC area (deterministic via setseed)
SELECT setseed(0.42);
INSERT INTO spatial_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.05 + random() * 0.2,
    40.65 + random() * 0.2
), 4326)
FROM generate_series(1, 100000)
ON CONFLICT DO NOTHING;

-- 1K random polygons (small squares)
INSERT INTO spatial_polygons (name, geom)
SELECT
    'poly_' || i,
    ST_SetSRID(ST_MakeEnvelope(
        -74.05 + random() * 0.18,
        40.65 + random() * 0.18,
        -74.05 + random() * 0.18 + 0.02,
        40.65 + random() * 0.18 + 0.02
    ), 4326)
FROM generate_series(1, 1000) AS s(i)
ON CONFLICT DO NOTHING;
