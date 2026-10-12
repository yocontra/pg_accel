SELECT s_name,count(*) AS numwait FROM tpch_supplier JOIN tpch_lineitem l ON l.l_suppkey=s_suppkey
JOIN tpch_orders ON o_orderkey=l.l_orderkey JOIN tpch_nation ON n_nationkey=s_nationkey
WHERE o_orderstatus='F' AND l.l_receiptdate>l.l_commitdate AND n_name='SAUDI ARABIA'
AND EXISTS (SELECT 1 FROM tpch_lineitem x WHERE x.l_orderkey=l.l_orderkey AND x.l_suppkey<>l.l_suppkey)
AND NOT EXISTS (SELECT 1 FROM tpch_lineitem x WHERE x.l_orderkey=l.l_orderkey AND x.l_suppkey<>l.l_suppkey AND x.l_receiptdate>x.l_commitdate)
GROUP BY s_name ORDER BY numwait DESC,s_name LIMIT 100
