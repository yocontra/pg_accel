SELECT c_name,c_custkey,o_orderkey,o_orderdate,o_totalprice,sum(l_quantity) AS quantity
FROM tpch_customer JOIN tpch_orders ON o_custkey=c_custkey JOIN tpch_lineitem ON l_orderkey=o_orderkey
WHERE o_orderkey IN (SELECT l_orderkey FROM tpch_lineitem GROUP BY l_orderkey HAVING sum(l_quantity)>300)
GROUP BY c_name,c_custkey,o_orderkey,o_orderdate,o_totalprice
ORDER BY o_totalprice DESC,o_orderdate,o_orderkey LIMIT 100
