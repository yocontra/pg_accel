SET pg_accel.enabled=off;
CREATE TEMP TABLE _count_rewrite(a int,b int,p bool,d date,e date);
INSERT INTO _count_rewrite VALUES(NULL,0,false,'infinity','2000-01-01');
EXPLAIN (VERBOSE,COSTS OFF) SELECT count(a IS DISTINCT FROM 1/b) FROM _count_rewrite;
EXPLAIN (VERBOSE,COSTS OFF) SELECT count(1/b IS NOT DISTINCT FROM a) FROM _count_rewrite;
DO $$
DECLARE predicate text; aggregate text; outcome text;
BEGIN
  FOREACH predicate IN ARRAY ARRAY['a IS DISTINCT FROM 1/b','1/b IS NOT DISTINCT FROM a',
    '(1/b>0) IS UNKNOWN','(1/b>0) IS NOT TRUE','a IS DISTINCT FROM (d-e)',
    'CASE WHEN p THEN a IS DISTINCT FROM 1/b ELSE false END'] LOOP
    FOREACH aggregate IN ARRAY ARRAY['count('||predicate||')','sum(CASE WHEN '||predicate||' THEN 1 ELSE 0 END)'] LOOP
      outcome := NULL;
      BEGIN EXECUTE 'SELECT '||aggregate||' FROM _count_rewrite';
      EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS outcome=RETURNED_SQLSTATE; END;
      RAISE NOTICE '% -> %',aggregate,COALESCE(outcome,'no error');
    END LOOP;
  END LOOP;
END $$;
