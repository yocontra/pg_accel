SELECT order_year,sum(CASE WHEN supplier_nation='BRAZIL' THEN volume ELSE 0 END)/sum(volume) AS market_share FROM (
 SELECT extract(year FROM o_orderdate) AS order_year,l_extendedprice*(1-l_discount) AS volume,sn.n_name AS supplier_nation
 FROM tpch_part JOIN tpch_lineitem ON l_partkey=p_partkey JOIN tpch_supplier ON s_suppkey=l_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_customer ON c_custkey=o_custkey
 JOIN tpch_nation cn ON cn.n_nationkey=c_nationkey JOIN tpch_region ON r_regionkey=cn.n_regionkey
 JOIN tpch_nation sn ON sn.n_nationkey=s_nationkey
 WHERE r_name='AMERICA' AND o_orderdate BETWEEN date '1995-01-01' AND date '1996-12-31'
 AND p_type='STANDARD ANODIZED STEEL'
) sales GROUP BY order_year ORDER BY order_year
