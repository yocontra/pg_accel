CREATE INDEX IF NOT EXISTS idx_spatial_points_geom ON spatial_points USING gist (geom);
CREATE INDEX IF NOT EXISTS idx_spatial_polygons_geom ON spatial_polygons USING gist (geom);
-- h3index supports btree and hash but not gist/spgist
CREATE INDEX IF NOT EXISTS idx_h3_cells_hash ON h3_cells USING hash (cell);
CREATE INDEX IF NOT EXISTS idx_analytics_events_ts ON analytics_events (ts);
CREATE INDEX IF NOT EXISTS idx_analytics_events_user ON analytics_events (user_id);
CREATE INDEX IF NOT EXISTS idx_analytics_events_value ON analytics_events (value);
