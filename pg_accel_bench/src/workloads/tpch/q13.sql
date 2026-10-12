SELECT order_count,count(*) AS customer_count FROM (
 SELECT c_custkey,count(o_orderkey) AS order_count FROM tpch_customer
 LEFT JOIN tpch_orders ON o_custkey=c_custkey AND o_comment NOT LIKE '%special%requests%'
 GROUP BY c_custkey
) distribution GROUP BY order_count ORDER BY customer_count DESC,order_count DESC
