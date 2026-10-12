SELECT ps_partkey,sum(ps_supplycost*ps_availqty) AS value
FROM tpch_partsupp JOIN tpch_supplier ON s_suppkey=ps_suppkey JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE n_name='GERMANY' GROUP BY ps_partkey
HAVING sum(ps_supplycost*ps_availqty)>(SELECT sum(ps_supplycost*ps_availqty)*0.0001
 FROM tpch_partsupp JOIN tpch_supplier ON s_suppkey=ps_suppkey JOIN tpch_nation ON n_nationkey=s_nationkey
 WHERE n_name='GERMANY') ORDER BY value DESC,ps_partkey
