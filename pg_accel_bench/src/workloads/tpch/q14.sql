SELECT 100.00*sum(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice*(1-l_discount) ELSE 0 END)
/sum(l_extendedprice*(1-l_discount)) AS promo_revenue
FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE l_shipdate>=date '1995-09-01' AND l_shipdate<date '1995-10-01'
