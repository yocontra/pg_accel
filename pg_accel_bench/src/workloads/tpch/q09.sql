SELECT nation,order_year,sum(amount) AS sum_profit FROM (
 SELECT n_name AS nation,extract(year FROM o_orderdate) AS order_year,
 l_extendedprice*(1-l_discount)-ps_supplycost*l_quantity AS amount
 FROM tpch_part JOIN tpch_lineitem ON l_partkey=p_partkey JOIN tpch_supplier ON s_suppkey=l_suppkey
 JOIN tpch_partsupp ON ps_partkey=l_partkey AND ps_suppkey=l_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_nation ON n_nationkey=s_nationkey
 WHERE p_name LIKE '%green%'
) profits GROUP BY nation,order_year ORDER BY nation,order_year DESC
