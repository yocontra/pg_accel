SELECT supplier_nation,customer_nation,ship_year,sum(volume) AS revenue FROM (
 SELECT sn.n_name AS supplier_nation,cn.n_name AS customer_nation,
 extract(year FROM l_shipdate) AS ship_year,l_extendedprice*(1-l_discount) AS volume
 FROM tpch_supplier JOIN tpch_lineitem ON l_suppkey=s_suppkey
 JOIN tpch_orders ON o_orderkey=l_orderkey JOIN tpch_customer ON c_custkey=o_custkey
 JOIN tpch_nation sn ON sn.n_nationkey=s_nationkey JOIN tpch_nation cn ON cn.n_nationkey=c_nationkey
 WHERE ((sn.n_name='FRANCE' AND cn.n_name='GERMANY') OR (sn.n_name='GERMANY' AND cn.n_name='FRANCE'))
 AND l_shipdate BETWEEN date '1995-01-01' AND date '1996-12-31'
) shipping GROUP BY supplier_nation,customer_nation,ship_year ORDER BY supplier_nation,customer_nation,ship_year
