SELECT sum(l_extendedprice)/7.0 AS avg_yearly FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE p_brand='Brand#23' AND p_container='MED BOX'
AND l_quantity<(SELECT 0.2*avg(x.l_quantity) FROM tpch_lineitem x WHERE x.l_partkey=p_partkey)
