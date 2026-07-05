SELECT setseed(0.42);
INSERT INTO analytics_events (user_id, event_type, value, ts)
SELECT
    (random() * 10000)::INTEGER,
    CASE (random() * 4)::INTEGER
        WHEN 0 THEN 'click'
        WHEN 1 THEN 'view'
        WHEN 2 THEN 'purchase'
        WHEN 3 THEN 'signup'
        ELSE 'other'
    END,
    random() * 1000,
    '2024-01-01'::timestamp + (random() * 365 * interval '1 day')
FROM generate_series(1, 1000000)
ON CONFLICT DO NOTHING;
