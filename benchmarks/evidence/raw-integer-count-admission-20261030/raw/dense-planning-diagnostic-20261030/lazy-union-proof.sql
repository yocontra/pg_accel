\set ON_ERROR_STOP on
BEGIN;
SELECT tests.t17_test_loaded_raw_quote_keeps_full_union_and_selected_type_me();
DO $$
BEGIN
  IF to_regclass('phase2_raw_quote_t') IS NULL THEN
    RAISE EXCEPTION 'mixed-union test returned before creating its fixture';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_accel_resident_status()
    WHERE relid = 'phase2_raw_quote_t'::regclass
      AND columns = ARRAY[1,2,3,4,5] AND pinned AND raw_bytes > 0
  ) THEN
    RAISE EXCEPTION 'mixed-union test did not retain its complete five-column owner';
  END IF;
  RAISE NOTICE 'mixed_union_positive_coverage=PASS columns=5';
END $$;
SELECT relid::regclass,columns,raw_bytes,pinned FROM pg_accel_resident_status()
WHERE relid = 'phase2_raw_quote_t'::regclass;
ROLLBACK;
