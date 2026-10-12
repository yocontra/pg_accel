SELECT n_name,sum(l_extendedprice*(1-l_discount)) AS revenue
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey
JOIN tpch_lineitem ON l_orderkey=o_orderkey
JOIN tpch_supplier ON s_suppkey=l_suppkey AND s_nationkey=c_nationkey
JOIN tpch_nation ON n_nationkey=s_nationkey
JOIN tpch_region ON r_regionkey=n_regionkey
WHERE r_name='ASIA' AND o_orderdate>=date '1994-01-01' AND o_orderdate<date '1995-01-01'
GROUP BY n_name ORDER BY revenue DESC,n_name
