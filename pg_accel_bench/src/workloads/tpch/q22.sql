SELECT substring(c_phone FROM 1 FOR 2) AS country_code,count(*) AS numcust,sum(c_acctbal) AS totacctbal
FROM tpch_customer WHERE substring(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17')
AND c_acctbal>(SELECT avg(c_acctbal) FROM tpch_customer WHERE c_acctbal>0 AND substring(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17'))
AND NOT EXISTS (SELECT 1 FROM tpch_orders WHERE o_custkey=c_custkey)
GROUP BY substring(c_phone FROM 1 FOR 2) ORDER BY country_code
