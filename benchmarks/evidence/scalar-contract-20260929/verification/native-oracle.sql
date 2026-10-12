\set ON_ERROR_STOP on
SET pg_accel.enabled=off;
BEGIN;
CREATE TEMP TABLE _spg_inputs(v0 int4,v1 int4);
INSERT INTO _spg_inputs VALUES(0,0);
UPDATE _spg_inputs SET v0=(2147483647)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) + (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'arithmetic_10_0: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) + (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_10_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-2147483648)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) - (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'arithmetic_11_2: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) - (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_11_3: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(2147483647)::int4,v1=(2)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) * (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'arithmetic_12_4: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(2)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) * (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_12_5: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-2147483648)::int4,v1=(-1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'arithmetic_13_6: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(-1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_13_7: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-2147483648)::int4,v1=(-1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '0'::text THEN
    RAISE EXCEPTION 'arithmetic_14_8: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(-1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_14_9: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(7)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22012' THEN
    RAISE EXCEPTION 'arithmetic_13_10: expected 22012, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_13_11: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(7)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22012' THEN
    RAISE EXCEPTION 'arithmetic_14_12: expected 22012, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_14_13: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '-2'::text THEN
    RAISE EXCEPTION 'arithmetic_13_14: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) / (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_13_15: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '-1'::text THEN
    RAISE EXCEPTION 'arithmetic_14_16: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) % (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_14_17: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-2147483648)::int4,v1=(2147483647)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) + (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '-1'::text THEN
    RAISE EXCEPTION 'arithmetic_10_18: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(2147483647)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) + (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'strict_null_10_19: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(2147483647)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS NOT TRUE THEN (42)::int4 ELSE ((v1) + ((1)::int4)) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'case_20: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(2147483647)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS NOT TRUE THEN (42)::int4 ELSE ((v1) + ((1)::int4)) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'case_21: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(2147483647)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS NOT TRUE THEN (42)::int4 ELSE ((v1) + ((1)::int4)) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'case_22: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(42)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN (v0) IS NOT NULL THEN v0 ELSE (((7)::int4) / (v1)) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'coalesce_23: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN (v0) IS NOT NULL THEN v0 ELSE (((7)::int4) / (v1)) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22012' THEN
    RAISE EXCEPTION 'coalesce_24: expected 22012, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) + (((v1) / ((0)::int4)))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22012' THEN
    RAISE EXCEPTION 'outer_null_error: expected 22012, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (NULL::boolean)::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'null_literal: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((21)::int4) + ((21)::int4)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'dup: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_30_0_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_0_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_30_0_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_0_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_30_0_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_0_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_30_1_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_1_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_30_1_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_1_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_30_1_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_30_1_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_30_2_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_30_2_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_30_2_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_30_2_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) AND ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_30_2_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (v0<>0) ELSE (((v0<>0)) AND ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_30_2_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'bool_31_0_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_0_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_31_0_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_0_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_31_0_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_31_0_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_31_1_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_1_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_31_1_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_1_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_31_1_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_1_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_31_2_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(0)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_31_2_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_31_2_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'lazy_bool_31_2_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) OR ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'bool_31_2_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (v0<>0) ELSE (((v0<>0)) OR ((v1<>0))) END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'lazy_bool_31_2_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) = ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_20_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) = ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_20_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) = (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_20_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) = (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_20_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) <> ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_21_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) <> ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_21_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) <> (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_21_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) <> (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_21_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) < ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_22_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) < ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_22_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) < (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_22_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) < (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_22_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) <= ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_23_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) <= ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_23_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) <= (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'compare_23_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) <= (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_23_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) > ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_24_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) > ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_24_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) > (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_24_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) > (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_24_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) >= ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_25_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(1)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) >= ((v1<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_25_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) >= (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'compare_25_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=(3)::int4;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) >= (v1)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'compare_null_25_2: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) IS NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'null_test_1_40_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) IS NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'null_test_1_40_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) IS NOT NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'null_test_1_41_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((((v0<>0)) IS NOT NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'null_test_1_41_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) IS NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'null_test_2_40_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) IS NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'null_test_2_40_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) IS NOT NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'null_test_2_41_0: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (((v0) IS NOT NULL))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'null_test_2_41_1: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-2147483648)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((-(v0)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS DISTINCT FROM '22003' THEN
    RAISE EXCEPTION 'unary_96: expected 22003, result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(-7)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((-(v0)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '7'::text THEN
    RAISE EXCEPTION 'unary_97: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((-(v0)))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'unary_98: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((NOT ((v0<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'false'::text THEN
    RAISE EXCEPTION 'unary_99: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((NOT ((v0<>0))))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM NULL::text THEN
    RAISE EXCEPTION 'unary_100: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '7'::text THEN
    RAISE EXCEPTION 'branch_52_101: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'branch_52_102: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS TRUE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '7'::text THEN
    RAISE EXCEPTION 'branch_52_103: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(0)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '42'::text THEN
    RAISE EXCEPTION 'branch_53_104: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=(1)::int4,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '7'::text THEN
    RAISE EXCEPTION 'branch_53_105: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT ((CASE WHEN ((v0<>0)) IS FALSE THEN (42)::int4 ELSE (7)::int4 END))::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM '7'::text THEN
    RAISE EXCEPTION 'branch_53_106: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
UPDATE _spg_inputs SET v0=NULL,v1=NULL;
DO $probe$
DECLARE actual text; actual_state text;
BEGIN
  BEGIN
    SELECT (true)::text INTO STRICT actual FROM _spg_inputs;
  EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
  END;
  IF actual_state IS NOT NULL OR actual IS DISTINCT FROM 'true'::text THEN
    RAISE EXCEPTION 'bool_literal: result=%, state=%',actual,actual_state;
  END IF;
END $probe$;
ROLLBACK;
\echo PGACCEL_SCALAR_NATIVE_ORACLE_OK:108
