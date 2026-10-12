SELECT sum(l_extendedprice*(1-l_discount)) AS revenue FROM tpch_lineitem JOIN tpch_part ON p_partkey=l_partkey
WHERE l_shipmode IN ('AIR','REG AIR') AND l_shipinstruct='DELIVER IN PERSON' AND (
 (p_brand='Brand#12' AND p_container IN ('SM CASE','SM BOX','SM PACK','SM PKG') AND l_quantity BETWEEN 1 AND 11 AND p_size BETWEEN 1 AND 5)
 OR (p_brand='Brand#23' AND p_container IN ('MED BAG','MED BOX','MED PKG','MED PACK') AND l_quantity BETWEEN 10 AND 20 AND p_size BETWEEN 1 AND 10)
 OR (p_brand='Brand#34' AND p_container IN ('LG CASE','LG BOX','LG PACK','LG PKG') AND l_quantity BETWEEN 20 AND 30 AND p_size BETWEEN 1 AND 15))
