SELECT s_acctbal, s_name, n_name, p_partkey, p_mfgr, s_address, s_phone, s_comment
FROM tpch_part JOIN tpch_partsupp ON ps_partkey=p_partkey
JOIN tpch_supplier ON s_suppkey=ps_suppkey
JOIN tpch_nation ON n_nationkey=s_nationkey
JOIN tpch_region ON r_regionkey=n_regionkey
WHERE p_size=15 AND p_type LIKE '%BRASS' AND r_name='EUROPE'
AND ps_supplycost=(SELECT min(x.ps_supplycost) FROM tpch_partsupp x
  JOIN tpch_supplier s ON s.s_suppkey=x.ps_suppkey
  JOIN tpch_nation n ON n.n_nationkey=s.s_nationkey
  JOIN tpch_region r ON r.r_regionkey=n.n_regionkey
  WHERE x.ps_partkey=p_partkey AND r.r_name='EUROPE')
ORDER BY s_acctbal DESC, n_name, s_name, p_partkey LIMIT 100
