SELECT setseed(0.42);
INSERT INTO h3_cells (cell, resolution, value)
SELECT
    h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(
        -74.05 + random() * 0.2,
        40.65 + random() * 0.2
    ), 4326)::point, r),
    r,
    random() * 1000
FROM generate_series(1, 10000) AS s(i),
     generate_series(5, 9) AS r_series(r)
ON CONFLICT DO NOTHING;
