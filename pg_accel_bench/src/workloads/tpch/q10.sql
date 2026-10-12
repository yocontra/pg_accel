SELECT c_custkey,c_name,sum(l_extendedprice*(1-l_discount)) AS revenue,c_acctbal,n_name,c_address,c_phone,c_comment
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey JOIN tpch_lineitem ON l_orderkey=o_orderkey
JOIN tpch_nation ON n_nationkey=c_nationkey
WHERE o_orderdate>=date '1993-10-01' AND o_orderdate<date '1994-01-01' AND l_returnflag='R'
GROUP BY c_custkey,c_name,c_acctbal,n_name,c_address,c_phone,c_comment
ORDER BY revenue DESC,c_custkey LIMIT 20
