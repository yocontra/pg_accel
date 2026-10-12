SELECT s_name,s_address FROM tpch_supplier JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE n_name='CANADA' AND s_suppkey IN (
 SELECT ps_suppkey FROM tpch_partsupp WHERE ps_partkey IN (SELECT p_partkey FROM tpch_part WHERE p_name LIKE 'forest%')
 AND ps_availqty>(SELECT 0.5*sum(l_quantity) FROM tpch_lineitem
 WHERE l_partkey=ps_partkey AND l_suppkey=ps_suppkey AND l_shipdate>=date '1994-01-01' AND l_shipdate<date '1995-01-01'))
ORDER BY s_name
