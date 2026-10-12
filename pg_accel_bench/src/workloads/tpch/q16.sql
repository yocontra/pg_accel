SELECT p_brand,p_type,p_size,count(DISTINCT ps_suppkey) AS supplier_count
FROM tpch_partsupp JOIN tpch_part ON p_partkey=ps_partkey
WHERE p_brand<>'Brand#45' AND p_type NOT LIKE 'MEDIUM POLISHED%'
AND p_size IN (49,14,23,45,19,3,36,9)
AND ps_suppkey NOT IN (SELECT s_suppkey FROM tpch_supplier WHERE s_comment LIKE '%Customer%Complaints%')
GROUP BY p_brand,p_type,p_size ORDER BY supplier_count DESC,p_brand,p_type,p_size
