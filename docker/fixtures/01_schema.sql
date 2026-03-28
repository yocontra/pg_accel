CREATE TABLE IF NOT EXISTS spatial_points (
    id SERIAL PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

CREATE TABLE IF NOT EXISTS spatial_polygons (
    id SERIAL PRIMARY KEY,
    name TEXT,
    geom geometry(Polygon, 4326) NOT NULL
);

CREATE TABLE IF NOT EXISTS h3_cells (
    id SERIAL PRIMARY KEY,
    cell h3index NOT NULL,
    resolution INTEGER NOT NULL,
    value DOUBLE PRECISION
);

CREATE TABLE IF NOT EXISTS analytics_events (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    value DOUBLE PRECISION,
    ts TIMESTAMP NOT NULL DEFAULT now()
);
