WITH revenue AS (
 SELECT l_suppkey AS supplier_key,sum(l_extendedprice*(1-l_discount)) AS total_revenue
 FROM tpch_lineitem WHERE l_shipdate>=date '1996-01-01' AND l_shipdate<date '1996-04-01' GROUP BY l_suppkey
)
SELECT s_suppkey,s_name,s_address,s_phone,total_revenue FROM tpch_supplier JOIN revenue ON supplier_key=s_suppkey
WHERE total_revenue=(SELECT max(total_revenue) FROM revenue) ORDER BY s_suppkey
